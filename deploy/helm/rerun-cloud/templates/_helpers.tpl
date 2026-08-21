{{/* 资源名前缀:release 名已含 chart 名时直接用 release 名(如 release 就叫 rerun-cloud) */}}
{{- define "rerun-cloud.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{/* 公共标签 */}}
{{- define "rerun-cloud.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/*
selector 标签:StatefulSet 的 selector 不可变,这组标签定死后不能再改,
所以只放最小集,不含版本号。组件名由调用方传入("rerun" / "curation")。
*/}}
{{- define "rerun-cloud.selectorLabels" -}}
app.kubernetes.io/name: {{ .root.Chart.Name }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{/* 凭证 Secret 名:existingSecret 优先,否则 chart 自己渲染的那个 */}}
{{- define "rerun-cloud.viewerSecretName" -}}
{{- .Values.secrets.existingSecret | default (printf "%s-secrets" (include "rerun-cloud.fullname" .)) -}}
{{- end -}}

{{/* token 签名密钥 Secret 名:existingTokenSecret 优先 */}}
{{- define "rerun-cloud.tokenSecretName" -}}
{{- .Values.secrets.existingTokenSecret | default (printf "%s-token-secret" (include "rerun-cloud.fullname" .)) -}}
{{- end -}}

{{/* TOS 挂载凭证 Secret 名(fsx CSI 用) */}}
{{- define "rerun-cloud.fsxSecretName" -}}
{{- .Values.daft.fsx.existingSecret | default (printf "%s-daft-fsx-key" (include "rerun-cloud.fullname" .)) -}}
{{- end -}}

{{/* ingressClass:显式配置优先,否则从 release 名派生,避免多实例撞 class */}}
{{- define "rerun-cloud.ingressClassName" -}}
{{- .Values.apig.ingressClassName | default (printf "%s-apig" (include "rerun-cloud.fullname" .)) -}}
{{- end -}}

{{/* Ingress host(网关分流键,非真实 DNS) */}}
{{- define "rerun-cloud.webHost" -}}
{{- .Values.apig.webHost | default (printf "%s-web.apig.internal" (include "rerun-cloud.fullname" .)) -}}
{{- end -}}
