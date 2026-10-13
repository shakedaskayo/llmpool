---
title: Team server
permalink: /team/
---
# Team server

A team can run one llmpool as a server that holds the pool. Each member's own llmpool keeps working as before and adds what the server offers.

## Joining

In the app: **Team**, enter the server's address, and sign in or paste an API key.

```bash
llmpool team connect https://pool.example.com         # opens the sign-in page
llmpool team connect https://pool.example.com --key   # asks for an API key
llmpool team                                          # the link, and any problem with it
llmpool team disconnect
```

In Claude Code: `/llmpool team https://pool.example.com`.

![Joining a team's pool]({{ '/assets/team.png' | relative_url }})

## What a member gets

The team's accounts appear tagged `team`, read-only. The team's rules are tried before the member's own. A member's own login of the same account always wins over the team's. Every minute the member's llmpool tells the server which team accounts it runs on, so `least_busy` and `pressure` rules see the whole team.

The server works in one of two ways:

| | Lending (default) | [Gateway]({{ site.baseurl }}/gateway/) |
|---|---|---|
| What reaches a member's machine | Access tokens and keys | Nothing but the member's own session |
| Who switches accounts | The member's llmpool | The server, per request |
| Refresh tokens | Stay on the server | Stay on the server |

When lending, a subscription login in use by one member is not lent to another until they stop, unless you turn sharing on.

## Running it

```bash
LLMPOOL_PUBLIC_URL=https://pool.example.com \
LLMPOOL_OIDC_CLIENT_ID=... LLMPOOL_OIDC_CLIENT_SECRET=... \
LLMPOOL_ALLOWED_DOMAINS=example.com \
LLMPOOL_ADMINS=lead@example.com \
LLMPOOL_VAULT_KEY="$(openssl rand -hex 24)" \
llmpool serve --listen 127.0.0.1:8787
```

| Variable | Meaning |
|---|---|
| `LLMPOOL_PUBLIC_URL` | Where members reach the server. Required |
| `LLMPOOL_OIDC_ISSUER` | Any OpenID Connect provider. Default `https://accounts.google.com` |
| `LLMPOOL_OIDC_CLIENT_ID`, `LLMPOOL_OIDC_CLIENT_SECRET` | Turn sign-in on. Register `{public url}/auth/callback` with the provider |
| `LLMPOOL_ALLOWED_DOMAINS`, `LLMPOOL_ALLOWED_EMAILS` | Who may sign in. Required with sign-in; checked on every request |
| `LLMPOOL_API_KEYS` | `name:key` pairs for members without sign-in; keys of 24+ characters |
| `LLMPOOL_ADMINS` | Members who may run the server from its web page |
| `LLMPOOL_ACCESS` | Who may use what, by tag: `*@example.com=work;lead@example.com=*;ci=ci` |
| `LLMPOOL_BLOCKED` | Members turned away whatever else admits them |
| `LLMPOOL_SHARE_LOGINS` | `1` lets several members run on one subscription login at once |
| `LLMPOOL_GATEWAY` | `1` makes the server a gateway |
| `LLMPOOL_VAULT_KEY` | 32+ random characters; stored logins are encrypted with it |

The server speaks plain HTTP: put TLS in front of it. Members' clients refuse an address that is not `https` unless it is on their own machine. For a cluster, see [Kubernetes]({{ '/kubernetes/' | relative_url }}).

### With Google

In Google Cloud, create an OAuth client of type **Web application** with the redirect address `https://pool.example.com/auth/callback`. Put its id and secret in the two `OIDC` variables and your domain in `LLMPOOL_ALLOWED_DOMAINS`. The issuer is Google's by default. Okta, Keycloak, Entra and Dex work the same way with `LLMPOOL_OIDC_ISSUER` set.

## Putting accounts in

On a machine that has the logins:

```bash
llmpool export pool.age        # asks for a passphrase
```

Then import that file on the server's page (**Accounts → Add account → Logins from a pool file**) or with `llmpool import pool.age` on the server. Keys can be added on the page directly.

## The server's own page

Open the server's address in a browser. Administrators sign in the way members do.

![The server's page]({{ '/assets/server.png' | relative_url }})

It is the same interface as the app, for the server's pool: accounts, the team's rules, and a **Server** page with the gateway switch and its recent requests, lending, who may use what, members, who runs on what, and the audit log. What is changed there is kept on the server and wins over the variables above.

![Members and the audit log]({{ '/assets/server-members.png' | relative_url }})
