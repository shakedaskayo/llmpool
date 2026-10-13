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

## Inviting people

On the server's page, **Server → Invite your team** gives you a link to copy or email. It opens a page that says who invited them to which pool, with one line to run:

![The invitation page]({{ '/assets/invite.png' | relative_url }})

```bash
curl -fsSL https://raw.githubusercontent.com/shakedaskayo/llmpool/main/scripts/install.sh | sh && ~/.local/bin/llmpool team join https://pool.example.com
```

That installs llmpool, connects it to the pool (their browser opens for sign-in), has new sessions use the pool, and makes plain `claude` start through it. They open a new terminal, type `claude`, and are on the pool. Someone who already has llmpool runs only `llmpool team join https://pool.example.com`.

The page is public and holds no secret: who gets in is still decided by sign-in and your allow list.

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

Anyone named in `LLMPOOL_ADMINS` adds accounts, from wherever is handy.

**A key**, on the server's page: **Accounts → Add account → API key**. Or from your machine:

```bash
llmpool add key openai --to-team
```

**A subscription login**, from a machine with llmpool and the agent on it. This signs in to the account in your browser and puts the login straight into the pool:

```bash
llmpool team connect https://pool.example.com     # once
llmpool add claude --to-team
```

The server's page shows these two lines to copy (**Add account → A login, from your machine**) and says when the account has arrived.

**An account you already have**: **Share with team** on it in the app, or

```bash
llmpool team share work-2
```

A login is moved, because only one place can renew it: once the server has it, your machine gets it back the way every member does. A key is copied. The login an agent on your machine is running on right now stays put; sign in to it as a separate account with `--to-team` instead.

**A whole pool at once**: `llmpool export pool.age` on the machine that has it, then **Add account → A pool file** on the server's page.

## The server's own page

Open the server's address in a browser. Administrators sign in the way members do.

![The server's page]({{ '/assets/server.png' | relative_url }})

It is the same interface as the app, for the server's pool: accounts, the team's rules, and a **Server** page with the gateway switch and its recent requests, lending, who may use what, members, who runs on what, and the audit log. What is changed there is kept on the server and wins over the variables above.

![Members and the audit log]({{ '/assets/server-members.png' | relative_url }})
