{{/* 会话名:rerun-native-<release 名> */}}
{{- define "rerun-native-session.name" -}}
{{- printf "rerun-native-%s" .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "rerun-native-session.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "rerun-native-session.host" -}}
{{- .Values.ingress.host | default (printf "%s.apig.internal" (include "rerun-native-session.name" .)) -}}
{{- end -}}
