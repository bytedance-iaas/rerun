{{/* Session name: rerun-native-<release name> */}}
{{- define "rerun-native-session.name" -}}
{{- printf "rerun-native-%s" .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "rerun-native-session.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/*
The image reference. The chart ships no default tag, so catch it here: an empty tag would otherwise
render as "repo:" and surface much later as a confusing ImagePullBackOff.
*/}}
{{- define "rerun-native-session.image" -}}
{{- if not .Values.image.repository }}
{{- fail "image.repository is required (the rerun image). Pointing -f at dataverse's values file supplies it." }}
{{- end }}
{{- if not .Values.image.tag }}
{{- fail "image.tag is required (the rerun image tag). Pointing -f at dataverse's values file supplies it." }}
{{- end }}
{{- printf "%s:%s" .Values.image.repository .Values.image.tag }}
{{- end -}}

{{/*
The session runs inside the cloud, so its TOS endpoint is the internal one — derived from the
region, following Volcengine's own naming, unless the deployment overrides it.
*/}}
{{- define "rerun-native-session.tos.endpoint" -}}
{{- if .Values.tos.endpointInternal }}
{{- .Values.tos.endpointInternal }}
{{- else if .Values.tos.region }}
{{- printf "https://tos-s3-%s.ivolces.com" .Values.tos.region }}
{{- else }}
{{- fail "tos.region is required: the TOS endpoint is derived from it (https://tos-s3-<region>.ivolces.com). Override tos.endpointInternal only when that pattern does not fit." }}
{{- end }}
{{- end -}}

{{- define "rerun-native-session.host" -}}
{{- .Values.ingress.host | default (printf "%s.apig.internal" (include "rerun-native-session.name" .)) -}}
{{- end -}}

{{/*
The session password. Always a Secret you created: a session with no password is an open remote
desktop, and ingress.enabled defaults to true, which puts it on the public internet.
*/}}
{{- define "rerun-native-session.password.validate" -}}
{{- if not .Values.existingPasswordSecret }}
{{- fail "existingPasswordSecret is required: the name of a Secret in this namespace carrying a session_password key.\n  kubectl -n <namespace> create secret generic <release>-vnc --from-literal=session_password=<password>\nThe chart deliberately takes no plaintext password: Helm stores values verbatim in the release history, where anyone who can run `helm get values` could read it back. Without a password the noVNC desktop starts unauthenticated, and ingress.enabled=true publishes it." }}
{{- end }}
{{- end -}}
