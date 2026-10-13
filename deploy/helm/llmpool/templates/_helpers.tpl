{{- define "llmpool.name" -}}
{{- .Release.Name | trunc 50 | trimSuffix "-" }}-llmpool
{{- end }}

{{- define "llmpool.labels" -}}
app.kubernetes.io/name: llmpool
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "llmpool.secret" -}}
{{- .Values.existingSecret | default (include "llmpool.name" .) }}
{{- end }}

{{/* Refuse to render a server nobody can reach safely or nobody can enter. */}}
{{- define "llmpool.validate" -}}
{{- if not (hasPrefix "https://" .Values.publicUrl) }}
{{- fail "publicUrl is required and must start with https://" }}
{{- end }}
{{- if and (not .Values.existingSecret) (not .Values.oidc.clientId) (not .Values.apiKeys) }}
{{- fail "set oidc.clientId (sign-in), apiKeys, or existingSecret: with none, nobody can connect" }}
{{- end }}
{{- if and .Values.oidc.clientId (not .Values.existingSecret) (not .Values.oidc.clientSecret) }}
{{- fail "oidc.clientSecret is required with oidc.clientId (or provide existingSecret)" }}
{{- end }}
{{- if and .Values.oidc.clientId (not .Values.oidc.allowedDomains) (not .Values.oidc.allowedEmails) }}
{{- fail "set oidc.allowedDomains or oidc.allowedEmails: with neither, nobody may sign in" }}
{{- end }}
{{- if and .Values.vaultKey (lt (len .Values.vaultKey) 32) }}
{{- fail "vaultKey is shorter than 32 characters" }}
{{- end }}
{{- if and .Values.networkPolicy.enabled (not .Values.networkPolicy.ingressNamespaces) }}
{{- fail "networkPolicy.ingressNamespaces is empty: name the namespace of your ingress controller, or turn the policy off" }}
{{- end }}
{{- range $name, $key := .Values.apiKeys }}
{{- if lt (len $key) 24 }}
{{- fail (printf "apiKeys.%s is shorter than 24 characters" $name) }}
{{- end }}
{{- end }}
{{- end }}
