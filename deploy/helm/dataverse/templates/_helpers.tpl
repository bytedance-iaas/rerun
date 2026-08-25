{{/*
Resource-name prefix. The release name as-is when it already contains the chart name (a release
simply called `dataverse`), otherwise <release>-dataverse — so two installs in one namespace never
collide.
*/}}
{{- define "dataverse.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{/*
ReRun component resource name. The bundle (chart/release, the shared gateway and Secrets) is
`dataverse`, but the web-viewer + catalog workload keeps its own identity `rerun-cloud` — so its
StatefulSet, pod (`rerun-cloud-0`), Services and PVC (`server-data-rerun-cloud-0`) all carry this
name, while the curation console and the gateway stay `dataverse-*`.
*/}}
{{- define "dataverse.rerunName" -}}
rerun-cloud
{{- end -}}

{{/* Labels shared by every object */}}
{{- define "dataverse.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/*
Selector labels. A StatefulSet's selector is IMMUTABLE, so this set can never change once it ships
— keep it minimal and free of the version. The caller passes the component name
("rerun" / "curation" / "vllm").
*/}}
{{- define "dataverse.selectorLabels" -}}
app.kubernetes.io/name: {{ .root.Chart.Name }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{/* ── Images ──────────────────────────────────────────────────────────────────────────────── */}}

{{/*
The ReRun image reference, as one full string. The chart ships no default, and the reference must
carry an explicit tag (or digest) — an untagged one silently means `latest`, which no build ever
pushes, and would surface much later as a confusing ImagePullBackOff.
*/}}
{{- define "dataverse.image" -}}
{{- if not .Values.image.rerun }}
{{- fail "image.rerun is required: the full rerun image reference, e.g. <registry>/rerun:<tag>. The chart does not track the image version." }}
{{- end }}
{{- if not (splitList "/" .Values.image.rerun | last | contains ":") }}
{{- fail (printf "image.rerun must include an explicit tag (got %q): an untagged reference means `latest`, which is never published." .Values.image.rerun) }}
{{- end }}
{{- .Values.image.rerun }}
{{- end -}}

{{/*
The curation console image, same rules. A separate project on its own release cadence, so its tag
never follows the rerun one.
*/}}
{{- define "dataverse.curator.image" -}}
{{- if not .Values.image.curator }}
{{- fail "image.curator is required when curator.enabled=true: the full robot_curator image reference, e.g. <registry>/robot_curator:<tag>. Set curator.enabled=false to deploy without the curation console." }}
{{- end }}
{{- if not (splitList "/" .Values.image.curator | last | contains ":") }}
{{- fail (printf "image.curator must include an explicit tag (got %q): an untagged reference means `latest`, which is never published." .Values.image.curator) }}
{{- end }}
{{- .Values.image.curator }}
{{- end -}}

{{- define "dataverse.vllm.image" -}}
{{- if not .Values.vllm.image.repository }}
{{- fail "vllm.image.repository is required when vllm.enabled=true." }}
{{- end }}
{{- printf "%s:%s" .Values.vllm.image.repository (.Values.vllm.image.tag | default "latest") }}
{{- end -}}

{{/* ── TOS endpoints ───────────────────────────────────────────────────────────────────────── */}}

{{/*
Volcengine names its TOS endpoints after the region, so one region is enough to derive both. An
explicit value in tos.* always wins, for the cases the pattern does not cover (an S3-compatible
endpoint, or a cluster with no internal route to TOS).
*/}}
{{- define "dataverse.tos.endpointPublic" -}}
{{- if .Values.tos.endpointPublic }}
{{- .Values.tos.endpointPublic }}
{{- else }}
{{- printf "https://tos-s3-%s.volces.com" (include "dataverse.tos.region" .) }}
{{- end }}
{{- end -}}

{{- define "dataverse.tos.endpointInternal" -}}
{{- if .Values.tos.endpointInternal }}
{{- .Values.tos.endpointInternal }}
{{- else }}
{{- printf "https://tos-s3-%s.ivolces.com" (include "dataverse.tos.region" .) }}
{{- end }}
{{- end -}}

{{- define "dataverse.tos.region" -}}
{{- if not .Values.tos.region }}
{{- fail "tos.region is required: the TOS endpoints are derived from it (https://tos-s3-<region>.volces.com and its internal .ivolces.com counterpart). Override tos.endpointPublic / tos.endpointInternal only when that pattern does not fit." }}
{{- end }}
{{- .Values.tos.region }}
{{- end -}}

{{/*
The endpoint presigned URLs are signed for. `public` and `internal` pick one of the two endpoints
above — the signature covers the host, so a URL signed for one network does not work on the other.
*/}}
{{- define "dataverse.presignEndpoint" -}}
{{- if eq .Values.catalog.presignNetwork "public" }}
{{- include "dataverse.tos.endpointPublic" . }}
{{- else if eq .Values.catalog.presignNetwork "internal" }}
{{- include "dataverse.tos.endpointInternal" . }}
{{- else }}
{{- fail (printf "catalog.presignNetwork must be \"public\" or \"internal\", got %q." .Values.catalog.presignNetwork) }}
{{- end }}
{{- end -}}

{{/* ── Secrets ─────────────────────────────────────────────────────────────────────────────── */}}

{{/*
The application-credentials Secret. Always one you created beforehand — the chart renders no Secret
of its own, because Helm keeps values verbatim in the release history where `helm get values` can
read them back.
*/}}
{{- define "dataverse.viewerSecretName" -}}
{{- if not .Values.secrets.existingSecret }}
{{- fail "secrets.existingSecret is required: the name of a Secret in this namespace carrying tos_access_key, tos_secret_key and, when web.basicAuth.enabled, web_htpasswd.\n  kubectl -n <namespace> create secret generic dataverse-secrets --from-literal=tos_access_key=<ak> --from-literal=tos_secret_key=<sk> --from-literal=web_htpasswd='<user>:<hash>'\nThe chart deliberately cannot build it from values: Helm stores values verbatim in the release history, where anyone who can run `helm get values` could read the credentials back." }}
{{- end }}
{{- .Values.secrets.existingSecret -}}
{{- end -}}

{{/*
The catalog's token-signing Secret, kept separate so it can be mounted into the catalog container
alone. Only consulted when token auth is on.
*/}}
{{- define "dataverse.tokenSecretName" -}}
{{- if not .Values.secrets.existingTokenSecret }}
{{- fail "secrets.existingTokenSecret is required when catalog.tokenAuth.enabled=true: the name of a Secret in this namespace carrying a server_token_secret key. Create it through a pipe so it never touches disk:\n  rerun server generate-secret | kubectl -n <namespace> create secret generic rerun-catalog-server-secrets --from-file=server_token_secret=/dev/stdin\nTo serve an unauthenticated catalog on a private network, set catalog.tokenAuth.enabled=false explicitly." }}
{{- end }}
{{- .Values.secrets.existingTokenSecret -}}
{{- end -}}

{{/* ── APIG ────────────────────────────────────────────────────────────────────────────────── */}}

{{/*
Name of the APIGInstance object the Ingresses bind to.
  create=true  -> the CR this chart renders, <release>-apig
  create=false -> the CR the platform already made for the adopted gateway, which it names
                  <instance-id>-apig-instance
*/}}
{{- define "dataverse.apig.instanceObjectName" -}}
{{- if .Values.apig.create }}
{{- printf "%s-apig" (include "dataverse.fullname" .) }}
{{- else }}
{{- printf "%s-apig-instance" .Values.apig.existingId }}
{{- end }}
{{- end -}}

{{/*
Ingress class. create=false: resolved from the cluster — the adopted gateway's APIGInstance
reports the platform id in status.id (spec.id only when it was itself adopted), and declares its
classes in spec.ingress.ingressClasses. lookup is live during install/upgrade (and template
--dry-run=server), and returns nothing when rendering offline — hence the explicit-value escape
hatch and the hard error, rather than a default that would leave the Ingress silently unclaimed.
*/}}
{{- define "dataverse.apig.ingressClassName" -}}
{{- if .Values.apig.ingressClassName }}
{{- .Values.apig.ingressClassName }}
{{- else if .Values.apig.create }}
{{- printf "%s-apig" (include "dataverse.fullname" .) }}
{{- else }}
{{- $found := "" }}
{{- range (lookup "loadbalancer.vke.volcengine.com/v1beta1" "APIGInstance" "" "").items }}
{{- if or (eq (dig "status" "id" "" .) $.Values.apig.existingId) (eq (dig "spec" "id" "" .) $.Values.apig.existingId) }}
{{- $found = dig "spec" "ingress" "ingressClasses" (list) . | first | default "" }}
{{- end }}
{{- end }}
{{- if $found }}
{{- $found }}
{{- else }}
{{- fail (printf "could not resolve the ingress class for apig.existingId=%q: no APIGInstance in the cluster reports this id (or it declares no ingress classes).\nDuring a real install/upgrade this means the id is wrong — list the gateways:\n  kubectl get apiginstance -A -o custom-columns=NAME:.metadata.name,ID:.status.id,CLASSES:.spec.ingress.ingressClasses\nWhen rendering offline (helm template/lint), the cluster is not reachable — render with --dry-run=server, or set apig.ingressClassName explicitly." .Values.apig.existingId) }}
{{- end }}
{{- end }}
{{- end -}}

{{/*
The one host every route in this chart shares — which is what puts the viewer, the console and the
catalog on a single public domain.
*/}}
{{- define "dataverse.apig.host" -}}
{{- default (printf "%s-web.apig.internal" (include "dataverse.fullname" .)) .Values.apig.host }}
{{- end -}}

{{/*
Binding annotations, derived from existingId. Empty until the gateway has an id — which for
create=true is only true after the gateway finishes provisioning.
*/}}
{{- define "dataverse.apig.annotations" -}}
{{- with .Values.apig.annotations }}
{{- toYaml . }}
{{- end }}
{{- with .Values.apig.existingId }}
ingress.vke.volcengine.com/apig-instance-name: {{ include "dataverse.apig.instanceObjectName" $ | quote }}
ingress.vke.volcengine.com/loadbalancer-id: {{ . | quote }}
{{- end }}
{{- end -}}

{{/*
Fail fast on the combinations that cannot work, so the error names the missing value instead of
surfacing later as an Ingress that never gets an address.
*/}}
{{- define "dataverse.apig.validate" -}}
{{- if .Values.apig.enabled }}
{{- if not .Values.web.basicAuth.enabled }}
{{- fail "web.basicAuth.enabled=true is required when publishing the viewer through APIG: the gateway does not authenticate (a gateway-level JWT would make the pages unopenable in a browser), so turning it off would put an open viewer on the public internet. Set apig.enabled=false for a private-network deployment." }}
{{- end }}
{{- if not .Values.catalog.tokenAuth.enabled }}
{{- fail "catalog.tokenAuth.enabled=true is required when publishing the catalog through APIG: its HTTP and gRPC routes share the public domain, and without token auth anyone who finds it can register and read datasets. Set apig.enabled=false for a private-network deployment." }}
{{- end }}
{{- if .Values.apig.create }}
{{- if not .Values.apig.subnetIds }}
{{- fail "apig.subnetIds is required when apig.create=true: the new gateway needs a subnet in this cluster's VPC. For HA prefer two subnets in different availability zones." }}
{{- end }}
{{- if .Values.apig.existingId }}
{{- fail "apig.existingId must be empty when apig.create=true. The provisioned gateway's id is reported in the APIGInstance's status.id and the Ingresses bind by ingress class, so nothing needs it back. Setting it writes spec.id, which is immutable — the admission webhook then rejects every upgrade with 'spec.id: Forbidden: forbidden to update'. Use existingId only with apig.create=false." }}
{{- end }}
{{- else }}
{{- if not .Values.apig.existingId }}
{{- fail "apig.existingId is required when apig.create=false: set it to the gateway's instance id from the APIG console, or set apig.create=true to provision a new gateway." }}
{{- end }}
{{- end }}
{{- end }}
{{- end -}}
