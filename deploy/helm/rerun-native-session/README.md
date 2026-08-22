# rerun-native-session Helm chart

A self-service, on-demand native viewer session in the cloud (one pod per person, deleted when done).
One release = one session (pod + service + optional Ingress), which lines up naturally with helm's lifecycle:
`helm install` opens a session, `helm uninstall` cleans it up in one go.

Prerequisite: the [`dataverse`](../dataverse/) chart (ReRun + the curation console) is deployed in the same namespace — its credentials Secret and APIG gateway are reused.

```sh
# Open a session (release name = your name, in lowercase letters/digits/dashes).
# The values layout is a subset of the dataverse chart's: point -f at the same
# values file and the image, cache bucket and credentials Secret all carry over,
# leaving only the session password to supply:
helm install qian deploy/helm/rerun-native-session -n rerun \
    -f deploy/secrets/values-prod.yaml \
    --set sessionPassword=<session password>

# List the running sessions (releases of the rerun-native-session chart)
helm list -n rerun

# Delete it when done
helm uninstall qian -n rerun
```

To stay off the public internet, use `--set ingress.enabled=false` and port-forward instead (the command is in the NOTES printed after installing).
When `dataverse`'s release is not named `dataverse`, `secrets.existingSecret` and `ingress.className` have to be set to the actual names.
