# dataverse Helm chart

The Helm chart for the always-on Dataverse cloud deployment: **one chart bundling two components, ReRun and the curation console**, plus the gateway entry point they share.

| Component | Contents |
|---|---|
| **ReRun** | The rerun StatefulSet (two containers: web viewer + catalog server) and its in-cluster Services (web / headless gRPC) |
| **Curation console** | The Daft StatefulSet (direct TOS access, reusing the AK/SK from the application Secret) and its site-configuration ConfigMap; `daft.enabled=false` turns the whole thing off |

Shared between them: the credentials and token-signing Secrets, the APIG gateway instance, and the path-routed Ingress (including the catalog's gRPC route).
The optional self-hosted vLLM (`vllm.enabled=true`) serves the curation console alone.

On-demand native viewer sessions in the cloud are a separate chart: [`../rerun-native-session`](../rerun-native-session/).

## Installing

The recommended flow — Secrets created with kubectl, zero secrets in values (full steps in docs/release/02-deploy.md):

```sh
# 1. Create the namespace and the two Secrets (application credentials / token
#    signing secret; the signing secret goes through a pipe so it never touches disk):
kubectl create namespace rerun
kubectl -n rerun create secret generic dataverse-secrets \
    --from-literal=tos_access_key=… --from-literal=tos_secret_key=… \
    --from-literal=web_htpasswd="alice:$(openssl passwd -apr1 'pw123')" \
    --from-literal=ark_api_key=…   # optional: only for the Volcengine Ark VLM backend
rerun server generate-secret | kubectl -n rerun create secret generic \
    rerun-catalog-server-secrets --from-file=server_token_secret=/dev/stdin

# 2. values only references those names (secrets.existingSecret /
#    secrets.existingTokenSecret) alongside the non-secret configuration; then install:
helm install dataverse deploy/helm/dataverse -n rerun -f deploy/secrets/values-prod.yaml

# 3. Follow the NOTES printed at the end of the install for the post-deployment
#    steps (find the domain, configure CORS, issue tokens).
```

For development you can skip the Secrets entirely and fill in the `secrets.*` fields for the chart to render (see the comments in values.yaml).
The signing secret is mounted into the catalog container as a 0400 file (a projected volume) and never as an environment variable, so the web and native sessions cannot see it; tokens are issued inside the container with `kubectl exec`, and the secret never leaves the cluster.

Name the release `dataverse`: the resource-name prefix is the release name, which keeps it consistent with the names used throughout the docs.

## Common switches

| value | Effect |
|---|---|
| `daft.enabled=false` | Do not deploy the curation console (which also skips its site configuration and the `/curation` route), leaving ReRun alone |
| `vllm.enabled=true` | Deploy a self-hosted vLLM (GPU) in the same namespace and register it automatically as one of the console's VLM backends |
| `secrets.arkApiKey` / `daft.arkBaseUrl` | Wire the console to the Volcengine Ark VLM backend: the API key (a secret, injected as `ARK_API_KEY`) plus the base url (ordinary configuration, injected as `ARK_BASE_URL`). With `existingSecret`, add the key to that Secret instead (as `ark_api_key`) |
| `vllm.model` / `vllm.servedModelName` / `vllm.gpuCount` / `vllm.nodeHostname` | Pick the model, its advertised name, the GPU count, and which GPU node to pin to |
| `apig.enabled=false` | Create neither the gateway nor the Ingress (bring your own) |
| `apig.existingInstanceId` | Adopt an existing APIG instance rather than creating one (setting it makes `apig.subnetIds` unnecessary) |
| `web.basicAuth.enabled=false` / `catalog.tokenAuth.enabled=false` | Turn authentication off (private-network debugging only) |
| `secrets.existingSecret` | Secrets come from an external Secret and the chart renders no plaintext |
| `catalog.hfEndpoint=""` | Reach the official HuggingFace directly (clusters outside China) |

## Self-hosted vLLM (optional, for the curation console)

With `vllm.enabled=true` the chart brings up one more standalone vLLM `Deployment` + `Service` in the same namespace (a GPU is required).
The console only offers the backends listed in `site.yaml`'s `vlm_backends` — there is no k8s discovery — so turning this switch on also makes the chart inject that vLLM into `vlm_backends` under the `self-hosted` key (its endpoint pointing at the in-cluster DNS name, ending in `/v1`), and it becomes selectable in the console's UI without anyone copying an address by hand.

By default the weights are fetched into an `emptyDir` by an initContainer going through the internal `oniond` and the Volcengine mirrors (not into the image, not onto TOS; a pod rebuild skips idempotently based on `*.safetensors` / `*.aria2`).
To skip that flow, leave `vllm.weightFetch.image` empty and set `vllm.model` to an HF repo id so vLLM pulls from HF itself (reusing the `catalog.hfEndpoint` mirror and the `hf_token` from the shared secret).

The one thing that matters: `vllm.servedModelName` has to match the model name the console sends requests under — the injected backend's `model` field is taken from it, so it usually takes care of itself.
To change models, change `vllm.model` (the local path `/models/<weightFetch.modelName>`, matching `weightFetch.modelName`).
If a single GPU cannot hold the model, raise `vllm.gpuCount` to 2 and add `--tensor-parallel-size 2` to `vllm.extraArgs`.

## Upgrades and configuration changes

- `helm upgrade dataverse deploy/helm/dataverse -n rerun -f …` is all it takes;
  changes to Secret or site-configuration contents roll the pods automatically through the checksum annotations
  (with `secrets.existingSecret` the chart cannot see the contents, so after editing the external Secret, `kubectl rollout restart` by hand).
- **Fields not to change**: the StatefulSet's selector labels (k8s does not allow it — changing them means delete and recreate);
  `apig.webHost` and the ingressClass (an in-place change leaves the old ADDRESS behind and needs the Ingress recreated, and a new host means a new public domain).

## Uninstalling, and the data

- `helm uninstall dataverse -n rerun`.
- The catalog's data PVC (`server-data-rerun-cloud-0`) is managed by volumeClaimTemplates and is **not deleted on uninstall**; delete it by hand once you are sure.
- The curation console mounts no PV: its workspace is an emptyDir (dataset cache / batch staging / task state), so deleting the pod loses only the cache and the task history. Deliveries are uploaded to the user's bucket under a "write the completeness marker last" protocol and are never lost.
- Once the APIG gateway instance is deleted, the domain assigned to it stops working.

## Assumptions

- The cluster runs on Volcengine VKE and has the APIGInstance CRD installed (`loadbalancer.vke.volcengine.com/v1beta1`);
- when creating a gateway, `apig.subnetIds` needs at least one subnet from this cluster's VPC (the gateway is replicated for HA, so two subnets in different availability zones are recommended; adopting an existing instance through `apig.existingInstanceId` needs none);
- the robot_curator image is at least Daft repo commit 412b91ce8 (sub-path support).
