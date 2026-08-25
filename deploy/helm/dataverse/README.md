# Dataverse chart

Helm packaging of the always-on Dataverse cloud deployment: **one chart, two components** plus the
public entry point they share.

| Component | Objects |
|---|---|
| **ReRun** | A StatefulSet with two containers — the web viewer and the catalog server — its headless and web Services, and the catalog's data disk |
| **Curation console** | Its own StatefulSet, reading and writing TOS directly with the shared AK/SK, plus its site-configuration ConfigMap. `curator.enabled=false` drops it |

Shared between them: the two Secrets, the APIG gateway, and the path-routed Ingresses — including
the catalog's gRPC route. An optional self-hosted vLLM (`vllm.enabled=true`) serves the console
alone.

The public entry point is **optional and Volcengine-specific**: it renders APIG Ingresses and, if
asked, an `APIGInstance` CRD. On any other cluster set `apig.enabled=false` and everything else
still applies.

On-demand native viewer sessions are a separate chart: [`../rerun-native-session`](../rerun-native-session/).

## Install

Create the two Secrets first. The chart renders none of its own and refuses to install without
them — Helm keeps values verbatim in the release history, so a credential passed through values
stays readable to anyone who can run `helm get values`:

```bash
kubectl create namespace rerun

kubectl -n rerun create secret generic dataverse-secrets \
    --from-literal=tos_access_key=<ak> \
    --from-literal=tos_secret_key=<sk> \
    --from-literal=web_htpasswd="<user>:$(openssl passwd -apr1)"   # prompts for the password

rerun server generate-secret | kubectl -n rerun create secret generic \
    rerun-catalog-server-secrets --from-file=server_token_secret=/dev/stdin
```

Then install, naming those Secrets rather than their contents:

```bash
helm install dataverse deploy/helm/dataverse -n rerun \
  --set image.rerun=<registry>/rerun:<tag> \
  --set image.curator=<registry>/robot_curator:<tag> \
  --set secrets.existingSecret=dataverse-secrets \
  --set secrets.existingTokenSecret=rerun-catalog-server-secrets \
  --set apig.existingId=<gateway instance id>
```

In practice those flags live in a gitignored values file installed with `-f`. The image references
have no default the chart could guess, so a missing one fails at render time rather than deploying
something broken. The full step-by-step, including how to find the gateway id, is in
[`docs/release/02-deploy.md`](../../../docs/release/02-deploy.md).

**Name the release `dataverse`.** Object names derive from it, and it is what the documentation,
the deployment guide and the `rerun-native-session` chart's defaults assume. The one exception is
the ReRun workload itself, which is always `rerun-cloud` — its pod is `rerun-cloud-0` and its disk
`server-data-rerun-cloud-0` regardless of the release name.

The keys the application Secret must carry are `tos_access_key`, `tos_secret_key`, and
`web_htpasswd` whenever `web.basicAuth.enabled` (the viewer and the console share that one account
table). `hf_token` and `ark_api_key` are optional and read with `optional: true`, so leaving either
out simply leaves the matching feature off. Both Secrets have to live in the release's namespace —
Kubernetes does not let a pod reference a Secret from another one.

## One region, not four endpoints

`tos.region` is the only address you set. Everything else follows Volcengine's own naming:

| Derived | Value | Used by |
|---|---|---|
| public endpoint | `https://tos-s3-<region>.volces.com` | the browser, and clients outside the cloud |
| internal endpoint | `https://tos-s3-<region>.ivolces.com` | the catalog server and the curation console |
| presign endpoint | whichever `catalog.presignNetwork` names | the URLs handed to reading clients |

`tos.endpointPublic` / `tos.endpointInternal` override the derivation, and exist for the one case
that needs it: a cluster with no internal route to TOS, where the internal endpoint has to be set to
the public address.

`catalog.presignNetwork` is `public` (the default: training clients live outside the cloud) or
`internal` (every client is inside this VPC — faster, and no public traffic billed). A URL signed
for one network does not work on the other; the signature covers the host.

## Defaults worth knowing

| Value | Default | Notes |
|---|---|---|
| `image.rerun` / `image.curator` | **required** | Full tagged references; the chart tracks no image version |
| `secrets.existingSecret` | **required** | The chart renders no Secret of its own |
| `secrets.existingTokenSecret` | **required** | Unless `catalog.tokenAuth.enabled=false` |
| `tos.region` | `cn-beijing` | Both TOS endpoints are derived from it |
| `web.basicAuth.enabled` | `true` | htpasswd table shared with the console |
| `catalog.tokenAuth.enabled` | `true` | Signed catalog tokens |
| `catalog.presignNetwork` | `public` | Where the reading clients are |
| `catalog.storage` | `100Gi`, `ebs-essd` | Catalog database + file cache, kept on uninstall |
| `curator.enabled` | `true` | `false` also drops the `/curation` route |
| `vllm.enabled` | `false` | Needs a GPU |
| `apig.enabled` | `true` | The deployment's only public entry point |
| `apig.create` | `false` | Adopt a gateway by default; `true` provisions one |

## Public entry point (APIG) — two ways

The gateway terminates HTTPS on an auto-assigned `*.volceapi.com` domain and routes by host and
path. Everything shares one host, which is what puts the viewer, the console and the catalog on a
single domain:

```text
/                                         web viewer
/curation                                 curation console
/api  /catalog  /version                  catalog, plain HTTP
/rerun.cloud.v1alpha1.RerunCloudService   catalog, gRPC (a second Ingress: the backend-protocol
                                          annotation applies to a whole Ingress)
```

Authentication deliberately does **not** happen at the gateway — a gateway-level JWT would make the
pages unopenable in a browser — so the backends do it, and the chart refuses to render a public
entry point with `web.basicAuth.enabled=false` or `catalog.tokenAuth.enabled=false`.

| Value | New gateway | Existing gateway | Notes |
|---|---|---|---|
| `apig.enabled` | `true` | `true` | Off means no public entry point |
| `apig.create` | `true` | `false` | Picks which mode |
| `apig.subnetIds` | **required** | — | Subnets in this cluster's VPC |
| `apig.existingId` | **must stay empty** | **required** | Gateway instance id, from the APIG console |
| `apig.ingressClassName` | optional | optional | Auto-resolved from the cluster by `existingId` at install time; set it only to render offline or to override |
| `apig.host` | recommended | recommended | Internal placeholder host, unique per gateway. Defaults to `<release>-web.apig.internal` |

Missing values fail at render time with a message naming the value, not later as an Ingress that
silently never gets an address.

### A. Provision a new gateway

```yaml
apig:
  enabled: true
  create: true
  subnetIds:
    - subnet-xxxxxxxxxxxxxxxxxxxxx       # availability zone A, in this cluster's VPC
    - subnet-yyyyyyyyyyyyyyyyyyyyy       # availability zone B
  host: dataverse-web.apig.internal      # internal placeholder, unique per gateway
```

The gateway is replicated for HA, so two subnets in **different availability zones** let the
platform spread the replicas and their CLB across them. All of them must belong to *this* cluster's
VPC — a subnet copied from another cluster fails with "cannot be found from VPC" and stays Pending
forever. To find them, copy the `volcengine-loadbalancer-subnet-id` annotation off any healthy
LoadBalancer Service in the cluster.

One install is enough; there is nothing to feed back. Provisioning takes a few minutes:

```bash
kubectl get apiginstance dataverse-apig -n rerun
```

Once it reports `Running`, its id appears in `status.id` and the Ingresses pick the gateway up by
ingress class.

⚠️ **Do not copy that id into `apig.existingId`.** `existingId` writes `spec.id`, which the CRD
treats as immutable, and the admission webhook then rejects every subsequent upgrade:

```text
spec.id: Forbidden: forbidden to update, old: , new: <id>
```

The release stays `failed` until the value is removed again. The chart refuses this combination up
front. `existingId` belongs to `create: false` only.

Gateway sizing (`instanceSpecCode`, `clbSpecCode`, `replicas`, `publicNetworkBillingType`,
`publicNetworkBandwidth`) is all optional — empty means the platform picks. If you do want to
control it, `1c2g` with `traffic` billing and a 200 Mbps cap is plenty: the gateway proxies UI and
API traffic, never dataset bytes, which clients read from TOS directly through presigned URLs.

⚠️ A gateway provisioned this way is **deleted by `helm uninstall`**, taking its `*.volceapi.com`
domain with it. Set `apig.retainOnDelete=true` *before* the uninstall to keep it — Helm reads that
from the manifest recorded in the release, so annotating the live object afterwards does not save
it.

### B. Adopt an existing gateway

```yaml
apig:
  enabled: true
  create: false
  existingId: gd9xxxxxxxxxxxxxxxxxx      # instance id from the APIG console
  host: dataverse-web.apig.internal      # must not collide with a host already on it
```

The gateway's ingress class is looked up in the cluster by that id at install time (a wrong id
fails the install and lists the gateways). Offline rendering (`helm template` without a cluster)
cannot look anything up — pass `--dry-run=server`, or set `apig.ingressClassName` explicitly.

One step, and `helm uninstall` leaves the gateway alone. Use this for anything whose URL people have
bookmarked. The chart renders no `APIGInstance` in this mode — claiming a shared gateway into the
release would put it under `helm uninstall`.

⚠️ The gateway's name in the APIG console is frequently **not** the name Kubernetes shows. The
in-cluster object is `<instance-id>-apig-instance`, while the console lists whatever the gateway was
originally named. Match on the instance id, not the name.

## Self-hosted vLLM (optional)

`vllm.enabled=true` adds a standalone vLLM `Deployment` and `Service` in the same namespace (a GPU
is required). The console offers only the backends listed in its `site.yaml` — there is no
in-cluster discovery — so turning this on also injects that vLLM under the `self-hosted` key, with
its in-cluster endpoint, and it becomes selectable in the UI without anyone copying an address by
hand.

`vllm.servedModelName` has to match the name the console sends requests under; the injected
backend's `model` field is taken from it, so it usually takes care of itself. If a single GPU cannot
hold the model, raise `vllm.gpuCount` to 2 and add `--tensor-parallel-size 2` to `vllm.extraArgs`.

Weights are prefetched by default: an initContainer downloads them onto the node's local disk
through `oniond` before vLLM starts, so `vllm.enabled=true` needs no further values. The download
path is Volcengine-internal — on a cluster outside that network set `vllm.weightFetch.enabled=false`
and write an HF repo id into `vllm.model`, and vLLM pulls from HuggingFace itself. The download
container's image defaults to `image.curator` (which already carries the tooling);
`vllm.weightFetch.image` overrides it.

## Upgrades and configuration changes

`helm upgrade dataverse deploy/helm/dataverse -n rerun -f …` is all it takes. A site-configuration
change rolls the curation pod automatically through its checksum annotation.

**Rotated credentials do not roll anything.** The chart cannot see inside a Secret it did not
create, so editing one changes nothing until the pods restart:

```bash
kubectl rollout restart statefulset/rerun-cloud statefulset/dataverse-curation -n rerun
```

## Things that will bite you

**The catalog PVC is immutable.** `catalog.storage.size` and `className` live in a
`volumeClaimTemplate`, which Kubernetes forbids changing after creation — an upgrade that touches
them is rejected. Resize by expanding the PVC directly (if the class supports online expansion) and
updating the value to match, or `kubectl delete sts --cascade=orphan` before reinstalling.

**The catalog PVC survives uninstall.** `server-data-rerun-cloud-0` is left behind with the catalog
database and cache, and a reinstall rebinds it. Delete it by hand to reclaim the disk.

**The console keeps nothing.** Its workspace is an `emptyDir`, so deleting the pod loses the dataset
cache and the task history — never a delivery, which is uploaded to the user's bucket under a
"write the completeness marker last" protocol.

**APIG hosts are routing keys, not DNS.** The real URL is an auto-assigned `*.volceapi.com` domain,
visible only at <https://console.volcengine.com/veapig> → instance → service list. `kubectl` cannot
read it. Never put an assigned `*.volceapi.com` name into `apig.host`, and never change `apig.host`
on a live deployment: the Ingress keeps its old ADDRESS, and a new host means a new public domain
that bookmarks and bucket CORS rules have to follow.

**Do not verify APIG by curling the CLB IP with a Host header.** It answers 401 with error code
010002 for every host, working ones included. Test the assigned domain instead.

**Selector labels are immutable.** Changing them means deleting and recreating the StatefulSets.

## Assumptions

- The cluster runs on Volcengine VKE with the `APIGInstance` CRD installed
  (`loadbalancer.vke.volcengine.com/v1beta1`). An install failing with "no matches for kind
  APIGInstance" does not have it — set `apig.enabled=false` and bring your own ingress.
- The Namespace is deliberately not part of the chart (`--create-namespace` handles it), so
  `helm uninstall` can never take the whole namespace with it.
