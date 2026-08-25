# Rerun-native-session chart

A self-service, on-demand native viewer session in the cloud: one pod per person, deleted when
done. One release = one session (pod + Service + optional Ingress), which lines up with helm's own
lifecycle — `helm install` opens a session, `helm uninstall` cleans it up in one go.

The native viewer runs on a virtual display inside the pod and reaches the browser over noVNC. Use
it for the recordings the web viewer struggles with: multi-gigabyte files, where the data never has
to leave the cloud.

Prerequisite: the [`dataverse`](../dataverse/) chart is deployed **in the same namespace**. This
chart creates no Secret and no gateway — it reuses dataverse's credentials Secret and hangs its
Ingress off dataverse's APIG gateway.

## Install

```bash
# Release name = your name, in lowercase letters, digits and dashes.
# The session password comes from a Secret, so create that first:
kubectl -n rerun create secret generic qian-vnc --from-literal=session_password=<password>

# The values layout is a subset of the dataverse chart's, so pointing -f at the same values file
# carries over the image, the region and the credentials Secret name:
helm install qian deploy/helm/rerun-native-session -n rerun \
    -f deploy/secrets/values-prod.yaml \
    --set existingPasswordSecret=qian-vnc

helm list -n rerun          # the running sessions
helm uninstall qian -n rerun && kubectl -n rerun delete secret qian-vnc
```

`existingPasswordSecret` is required, and the chart takes no plaintext password — Helm stores
values verbatim in the release history, where `helm get values` can read one back. A session with
no password would be an open remote desktop, and `ingress.enabled` defaults to true.

To stay off the public internet entirely, `--set ingress.enabled=false` and use the port-forward
command the NOTES print after installing.

## What carries over from dataverse

| Value | Comes from | If dataverse's release is not named `dataverse` |
|---|---|---|
| `image.rerun` | the same field in dataverse's values | — |
| `tos.region`, `tos.rrdArtifactsUrl`, `catalog.hfEndpoint` | the same fields | — |
| `secrets.existingSecret` | dataverse's credentials Secret | set it to the actual Secret name |
| `ingress.className` | the class dataverse's gateway declares | dataverse's install notes print it (`class …`); with `apig.create=true` it is `<release>-apig` |

Fields dataverse has and this chart does not use (`curator.*`, `apig.*`, `vllm.*`, `catalog.storage`
and the rest) are simply ignored, which is what makes sharing one values file work.

Like dataverse, the TOS endpoint is derived from `tos.region`
(`https://tos-s3-<region>.ivolces.com`, the internal one — the session runs inside the cloud).
`tos.endpointInternal` overrides it for a cluster with no internal route to TOS.

## Things worth knowing

**Each session gets its own public domain.** Its host is a routing key, not DNS; the platform
assigns a `*.volceapi.com` domain per host, visible only in the APIG console
(<https://console.volcengine.com/veapig> → service list).

**The pod does not restart.** `restartPolicy: Never` — when the viewer exits, the session is over
and the stopped pod stays around so its logs can still be read. `helm uninstall` removes it.

**Sessions are billed while they run.** Nothing reaps them; how many can run at once is decided by
the namespace's resource quota. Uninstall when you are done.

**Nothing is persisted.** The session has no volume beyond `/dev/shm`. Anything worth keeping goes
back to TOS.
