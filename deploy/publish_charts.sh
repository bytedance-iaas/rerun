#!/usr/bin/env bash
# Package this repo's Helm charts and push them to the Volcengine OCI registry.
#
#   bash deploy/publish_charts.sh                    # package, log in, push both charts
#   DRY_RUN=1 bash deploy/publish_charts.sh          # package + lint only, no login, no push
#   CHARTS="dataverse" bash deploy/publish_charts.sh # restrict to one chart
#
# The repo ships two charts, both under deploy/helm:
#   dataverse             the always-on cloud deployment: ReRun (web viewer +
#                         catalog server) and the curation console bundled in one
#                         chart, plus the APIG gateway they share
#   rerun-native-session  the on-demand native-viewer session (one release per user)
#
# Everything deployment-specific comes from the environment, so this file holds no
# registry coordinates and no credentials:
#   HELM_REGISTRY_HOST       registry hostname, e.g. example.cr.volces.com
#   HELM_REGISTRY_NAMESPACE  namespace the charts are pushed under
#   HELM_REGISTRY_USERNAME   robot account      (not needed for DRY_RUN)
#   HELM_REGISTRY_PASSWORD   its password       (not needed for DRY_RUN)
#
# Optional:
#   CHARTS                   space-separated chart directory names under deploy/helm
#                            (default: all of them)
#   HELM_VERSION             helm to fetch when the runner has none (default: latest)
#
# Nothing here needs root or a package manager. The CI image runs as nobody on a
# release old enough that its package sources are gone, so the only things
# assumed present are bash, curl and tar.

set -euo pipefail

# Checked before anything else: a missing coordinate is a configuration error, and
# failing here costs nothing, whereas failing after the helm download wastes the
# whole setup. DRY_RUN needs these too, since it reports the push target.
: "${HELM_REGISTRY_HOST:?Set HELM_REGISTRY_HOST, the registry hostname (e.g. example.cr.volces.com).}"
: "${HELM_REGISTRY_NAMESPACE:?Set HELM_REGISTRY_NAMESPACE, the namespace the charts are pushed under.}"
registry="${HELM_REGISTRY_HOST}"
namespace="${HELM_REGISTRY_NAMESPACE}"

# Resolve from the script's own location so the working directory does not matter.
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
helm_dir="deploy/helm"

# Default to every chart directory in deploy/helm, so a chart added later is
# published without touching this script. Sorted for a stable, readable log.
if [[ -n "${CHARTS:-}" ]]; then
  read -r -a charts <<<"${CHARTS}"
else
  charts=()
  for dir in "${helm_dir}"/*/; do
    [[ -f "${dir}Chart.yaml" ]] && charts+=("$(basename -- "${dir}")")
  done
fi
[[ ${#charts[@]} -gt 0 ]] || { echo "error: no charts found under ${helm_dir}" >&2; exit 1; }

# helm lint refuses a chart whose `required` values are unset, and each chart
# demands a different set. These are lint-only placeholders — the real values come
# from the deploy-time values file, never from here.
lint_args_for() {
  case "$1" in
    dataverse)
      echo "--set image.repository=ci-lint --set image.tag=ci-lint" \
           "--set curator.image.repository=ci-lint --set curator.image.tag=ci-lint" \
           "--set apig.existingId=ci-lint --set apig.ingressClassName=ci-lint" \
           "--set secrets.existingSecret=ci-lint --set secrets.existingTokenSecret=ci-lint"
      ;;
    rerun-native-session)
      echo "--set image.repository=ci-lint --set image.tag=ci-lint --set existingPasswordSecret=ci-lint"
      ;;
    *) echo "" ;;
  esac
}

echo "=== environment ==="
echo "user:   $(id -un 2>/dev/null || echo unknown) (uid $(id -u))"
echo "os:     $(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}" || uname -s) $(uname -m)"
echo "helm:   $(command -v helm >/dev/null 2>&1 && helm version --short 2>&1 || echo MISSING)"
echo "curl:   $(command -v curl || echo MISSING)"
echo "tar:    $(command -v tar || echo MISSING)"
echo "charts: ${charts[*]}"
echo "==================="

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

# helm ships a statically linked binary, so unpacking it into the work directory
# needs no privileges — the only option available here, since the runner is
# nobody, sudo cannot elevate, and there is no usable package source.
if ! command -v helm >/dev/null 2>&1; then
  case "$(uname -m)" in
    x86_64 | amd64) helm_arch=amd64 ;;
    aarch64 | arm64) helm_arch=arm64 ;;
    *) echo "error: unsupported architecture $(uname -m)" >&2; exit 1 ;;
  esac
  helm_os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  helm_version="${HELM_VERSION:-$(curl -fsSL --max-time 30 https://get.helm.sh/helm-latest-version 2>/dev/null || echo v4.2.4)}"
  helm_url="https://get.helm.sh/helm-${helm_version}-${helm_os}-${helm_arch}.tar.gz"

  echo "helm missing, fetching ${helm_url}"
  mkdir -p "${work}/helm"
  curl -fsSL --max-time 300 "${helm_url}" | tar -xz -C "${work}/helm" ||
    { echo "error: could not download helm from ${helm_url}" >&2; exit 1; }
  export PATH="${work}/helm/${helm_os}-${helm_arch}:${PATH}"

  command -v helm >/dev/null 2>&1 ||
    { echo "error: helm still not on PATH after unpacking" >&2; exit 1; }
  echo "helm ready: $(helm version --short 2>&1)"
fi

# Lint and package every chart before pushing any of them: a chart that fails to
# build should not leave half the set published, since the two are deployed
# together and a partial push is worse than no push at all.
# Name and version come from each Chart.yaml, which is also where helm reads them,
# so a package is simply whatever lands in its otherwise empty directory. That
# keeps chart identity in one place and this script out of the business of
# parsing YAML.
packages=()
for chart in "${charts[@]}"; do
  chart_dir="${helm_dir}/${chart}"
  [[ -f "${chart_dir}/Chart.yaml" ]] ||
    { echo "error: ${chart_dir}/Chart.yaml not found" >&2; exit 1; }

  echo "=== packaging ${chart} ==="
  # Word splitting is what carries the per-chart --set flags here, hence no quotes.
  # shellcheck disable=SC2046
  helm lint "${chart_dir}" $(lint_args_for "${chart}")
  # `helm lint` is not enough on its own: on Helm 4 an unset `required` / a `fail`
  # only logs at INFO and lint still exits 0, and `helm package` never renders the
  # templates at all. So render here too — helm template exits non-zero on those,
  # which is what actually keeps a chart that cannot build out of the registry.
  # shellcheck disable=SC2046
  helm template "${chart}" "${chart_dir}" $(lint_args_for "${chart}") >/dev/null
  mkdir -p "${work}/pkg/${chart}"
  helm package "${chart_dir}" --destination "${work}/pkg/${chart}"
  packages+=("$(echo "${work}/pkg/${chart}"/*.tgz)")
done

if [[ "${DRY_RUN:-0}" == "1" ]]; then
  for package in "${packages[@]}"; do
    echo "dry run: would push ${package##*/} to oci://${registry}/${namespace}"
  done
  exit 0
fi

: "${HELM_REGISTRY_USERNAME:?Set HELM_REGISTRY_USERNAME (registry robot account).}"
: "${HELM_REGISTRY_PASSWORD:?Set HELM_REGISTRY_PASSWORD.}"

# Keep the login in a throwaway config: the default ~/.config/helm would leave
# the robot credentials on a reused CI runner for the next job to find. One login
# covers every push below, since all charts go to the same registry.
config="${work}/registry.json"
printf '%s' "${HELM_REGISTRY_PASSWORD}" |
  helm registry login "${registry}" --username "${HELM_REGISTRY_USERNAME}" \
    --password-stdin --registry-config "${config}"

# helm push reports the pushed reference and its digest on success.
for package in "${packages[@]}"; do
  echo "=== pushing ${package##*/} ==="
  helm push "${package}" "oci://${registry}/${namespace}" --registry-config "${config}"
done
