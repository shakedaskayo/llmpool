---
title: Kubernetes
permalink: /kubernetes/
---
# On Kubernetes

```bash
helm install pool oci://ghcr.io/shakedaskayo/charts/llmpool \
  --set publicUrl=https://pool.example.com \
  --set oidc.clientId=... --set oidc.clientSecret=... \
  --set 'oidc.allowedDomains={example.com}' \
  --set 'admins={lead@example.com}' \
  --set ingress.enabled=true --set ingress.className=nginx
```

The image is `ghcr.io/shakedaskayo/llmpool`, for amd64 and arm64. To build your own: `docker build -t registry.example.com/llmpool:0.1.0 .` and set `image.repository`.

The notes the chart prints say how to import accounts.

## Values

| Value | Meaning |
|---|---|
| `publicUrl` | Where members reach the server. Required, and `https` |
| `oidc.issuer`, `oidc.clientId`, `oidc.clientSecret` | Sign-in. Leave `clientId` empty for API keys only |
| `oidc.allowedDomains`, `oidc.allowedEmails` | Who may sign in |
| `apiKeys` | Members who connect with a key: name → key (24+ characters) |
| `admins` | Who may open the server's web page |
| `access`, `blocked` | The starting rules for who may use what |
| `gateway.enabled`, `gateway.upstreams` | Be a [gateway]({{ site.baseurl }}/gateway/); move providers' addresses |
| `shareLogins` | Let several members run on one subscription login |
| `existingSecret` | A Secret you manage, instead of secrets in values |
| `vaultKey` | The key stored logins are encrypted with; made at install if empty |
| `persistence.*` | The volume the pool lives on |
| `ingress.*`, `networkPolicy.*` | How traffic gets in |

## What the chart takes care of

- **One writer.** One replica with the `Recreate` strategy: the pool's state and its single-use refresh tokens allow no second copy.
- **Least privilege.** Non-root, read-only root filesystem, all capabilities dropped, no service account token.
- **The pool outlives the release.** The volume and the Secret holding the vault key both survive `helm uninstall`; without the key the volume cannot be read.
- **Refuses unsafe installs.** No `https`, nobody who may connect, sign-in without an allow list, or a short key: the chart will not render.

The pod needs to reach your sign-in provider and the LLM providers. Put TLS on the ingress.
