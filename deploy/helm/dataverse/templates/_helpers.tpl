{{/* Resource-name prefix: use the release name as-is when it already contains the chart name (e.g. a release simply called `dataverse`) */}}
{{- define "dataverse.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{/*
ReRun component resource name. The bundle (chart/release, the shared gateway and
Secrets) is `dataverse`, but the ReRun web-viewer + catalog workload keeps its own
identity `rerun-cloud` — so its StatefulSet, pod (`rerun-cloud-0`), its Services
and its PVC (`server-data-rerun-cloud-0`) all carry this name, while the Daft
console and the gateway stay `dataverse-*`.
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
Selector labels: a StatefulSet's selector is immutable, so this set can never
change once it ships — keep it minimal and free of the version. The caller
passes the component name ("rerun" / "curation").
*/}}
{{- define "dataverse.selectorLabels" -}}
app.kubernetes.io/name: {{ .root.Chart.Name }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{/* Credentials Secret: existingSecret wins, otherwise the one this chart renders */}}
{{- define "dataverse.viewerSecretName" -}}
{{- .Values.secrets.existingSecret | default (printf "%s-secrets" (include "dataverse.fullname" .)) -}}
{{- end -}}

{{/* Token-signing-secret name: existingTokenSecret wins */}}
{{- define "dataverse.tokenSecretName" -}}
{{- .Values.secrets.existingTokenSecret | default (printf "%s-token-secret" (include "dataverse.fullname" .)) -}}
{{- end -}}

{{/* TOS mount-credentials Secret (used by the fsx CSI) */}}
{{- define "dataverse.fsxSecretName" -}}
{{- .Values.daft.fsx.existingSecret | default (printf "%s-daft-fsx-key" (include "dataverse.fullname" .)) -}}
{{- end -}}

{{/* ingressClass: an explicit setting wins, otherwise derive it from the release name so two installs cannot claim the same class */}}
{{- define "dataverse.ingressClassName" -}}
{{- .Values.apig.ingressClassName | default (printf "%s-apig" (include "dataverse.fullname" .)) -}}
{{- end -}}

{{/* Ingress host (a gateway routing key, not real DNS) */}}
{{- define "dataverse.webHost" -}}
{{- .Values.apig.webHost | default (printf "%s-web.apig.internal" (include "dataverse.fullname" .)) -}}
{{- end -}}
