---
title: Security
permalink: /security/
---
# Security

## On your machine

- **Where secrets live.** Logins and keys are in the macOS Keychain (or the system keyring on Linux), never in llmpool's own files. Claude Code's login stays where Claude Code keeps it.
- **llmpool does not sign in for you.** An account is added by running the agent's own sign-in and taking the result. llmpool never sees a password.
- **One copy of a login.** A subscription login's refresh token is single-use, so llmpool keeps each account in at most one place at a time and reads it back before replacing it.
- **The local API** listens on loopback only, behind a token readable by you alone.
- **What accounts may do.** A key account sets environment variables that look like credentials and nothing else.

## On a server

- **Refresh tokens never leave it.** When lending, members get access tokens. As a [gateway]({{ '/gateway/' | relative_url }}), they get nothing at all.
- **Sign-in.** The server does the OpenID Connect code flow with the provider and hands the result to the member's llmpool on its own machine, bound to a secret only that llmpool holds. Issuer, audience, nonce, expiry and a verified email are checked; the allow list is checked again on every request.
- **Sessions** are random tokens; the server stores only their hashes.
- **The web page** is for members named as administrators. Its session is a cookie no script can read and no other site can send, and requests that change anything must carry a header only the page itself adds. Exporting logins and stopping the server cannot be done from a browser.
- **At rest.** With `LLMPOOL_VAULT_KEY`, every stored login is encrypted (XChaCha20-Poly1305) and bound to its account.
- **Audit.** Sign-ins, refusals, lent accounts, switches and administrators' changes are written to `team-audit.log` and standard error, never with a secret. Gateway traces never hold what was said.
- **Transport.** The server speaks HTTP; TLS is your ingress's. Clients refuse a server that is not `https` unless it is on their own machine.

## Limits worth knowing

- A member who is lent an access token can read it: they run the agent that uses it. Only the gateway keeps credentials from members.
- The ID token's signature is not verified separately: it is read only from the provider's token endpoint over verified TLS, which OpenID Connect allows.
- Rules on a member's machine are enforced by the member's llmpool. What a server controls for certain is what it hands out.

## Reporting a problem

Open an issue on GitHub for anything that is not sensitive. For a vulnerability, use GitHub's private vulnerability reporting on the repository.
