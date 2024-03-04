{{/*
Expand the name of the chart.
*/}}
{{- define "router.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "router.fullname" -}}
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
Create a name for our rhai config map.
*/}}
{{- define "router.rhaiConfigMapName" -}}
{{- printf "%s-rhai" (include "router.fullname" .) }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "router.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "router.labels" -}}
helm.sh/chart: {{ include "router.chart" . }}
{{ include "router.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "router.selectorLabels" -}}
app.kubernetes.io/name: {{ include "router.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "router.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "router.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Return secret name to be used based on provided values.
*/}}
{{- define "router.managedFederation.apiSecretName" -}}
{{- $fullName := include "router.fullname" . -}}
{{- default $fullName .Values.managedFederation.existingSecret | quote -}}
{{- end -}}

{{/*
Credit to Bitnami
https://github.com/bitnami/charts/blob/master/bitnami/common/templates/_tplvalues.tpl

Renders a value that contains template.
Usage:
{{ include "apollographql.tplvalues.render" ( dict "value" .Values.path.to.the.Value "context" $) }}
*/}}
{{- define "apollographql.tplvalues.render" -}}
    {{- if typeIs "string" .value }}
        {{- tpl .value .context }}
    {{- else }}
        {{- tpl (.value | toYaml) .context }}
    {{- end }}
{{- end -}}

{{- define "router.prometheusMetricsPath" -}}
{{/* NOTE: metrics configuration moved under telemetry.exporters in Router 1.35.0 */}}
{{- if (((((.Values.router).configuration).telemetry).exporters).metrics).prometheus }}
{{- .Values.router.configuration.telemetry.exporters.metrics.prometheus.path | quote }}
{{- else if ((((.Values.router).configuration).telemetry).metrics).prometheus }}
{{- .Values.router.configuration.telemetry.metrics.prometheus.path | quote }}
{{- else -}}
"/metrics"
{{- end }}
{{- end -}}

{{/*
This function takes the `extraLabels` map and templatizes the values.
This allows to pass references from the values and interprates them as template.
A use case for this usage is to add a custom label to the deployment referencing the version of the chart.
For example:

```
extraLabels:
  custom-version: {{ .Chart.AppVersion }}
```
*/}}
{{- define "apollographql.templatizeExtraLabels" -}}
{{- range $key, $value := .Values.extraLabels }}
{{ printf "%s: %s" $key (include  "apollographql.tplvalues.render" ( dict "value" $value "context" $))}}
{{- end -}}
{{- end -}}
