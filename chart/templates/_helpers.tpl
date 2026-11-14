{{/*
Expand the name of the chart.
*/}}
{{- define "gh-token.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "gh-token.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "gh-token.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels.
*/}}
{{- define "gh-token.labels" -}}
helm.sh/chart: {{ include "gh-token.chart" . }}
{{ include "gh-token.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels.
*/}}
{{- define "gh-token.selectorLabels" -}}
app.kubernetes.io/name: {{ include "gh-token.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
ServiceAccount name.
*/}}
{{- define "gh-token.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "gh-token.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Credentials secret name.
*/}}
{{- define "gh-token.secretName" -}}
{{- if .Values.secret.create }}
{{- default (printf "%s-credentials" (include "gh-token.fullname" .)) .Values.secret.name }}
{{- else }}
{{- required "secret.name is required when secret.create is false" .Values.secret.name }}
{{- end }}
{{- end }}
