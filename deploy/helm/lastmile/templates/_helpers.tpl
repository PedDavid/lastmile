{{- define "lastmile.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "lastmile.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "lastmile.labels" -}}
app.kubernetes.io/name: {{ include "lastmile.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end -}}

{{- define "lastmile.selectorLabels" -}}
app.kubernetes.io/name: {{ include "lastmile.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "lastmile.cookieSecretName" -}}
{{- default (printf "%s-cookie-key" (include "lastmile.fullname" .)) .Values.cookieKey.existingSecret -}}
{{- end -}}

{{- define "lastmile.claimName" -}}
{{- default (printf "%s-data" (include "lastmile.fullname" .)) .Values.persistence.existingClaim -}}
{{- end -}}
