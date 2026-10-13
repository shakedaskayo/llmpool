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

- **The gateway hands out models, nothing else.** Only requests to models are forwarded; an account's files, keys, billing and stored work at the provider are out of reach through it. A member cannot wear out or lock an account for the others: there is a limit on requests in flight per member, and a refusal provoked by one member's request sets the account aside for that member alone.
- **A member's word is taken within limits.** When lending, a member can hold at most three logins for themselves, and only members running on an account can report its usage.
- **A server sets only its provider's variables.** On a member's machine, an account from a server can set the variables that provider's agents read and no others.
- **Settings fail closed.** What an administrator changed is written whole or not at all, and a settings file that cannot be read stops the server instead of falling back to looser values.
- **Guessing is slow.** Tokens are 256 random bits and keys must be 24 characters or more; on top of that, a burst of wrong tokens gets everyone without a right one told to wait.
- **The page's own protections.** Every page is sent with a content security policy (only the server's own scripts run), no framing, no referrer, and strict transport security when served over `https`.

## Staying up

- **A slow client holds only itself.** Every request is taken off the few threads that accept connections; gateway requests and everything else have separate limits (`LLMPOOL_GATEWAY_MAX`, 256 by default), and beyond a limit the server answers 503 at once rather than queueing.
- **A provider having a bad moment is not the member's problem.** A request refused for rate, for the account, or by a provider error is tried on the next account, up to three. An account refused for its rate is left alone for as long as the provider says; one the provider no longer accepts is set aside for five minutes and an administrator is told in the audit log.
- **The decision model cannot slow or stop a request.** It is asked after the answer has gone out; without it the fixed rules decide.
- **Stopping is gentle.** On SIGTERM the server takes no new requests, lets answers in flight finish for up to 30 seconds, then saves and exits. The chart gives it 45.
- **Logs cannot fill the volume.** The audit and trace logs start afresh at 32 MB and keep one file before.
- **State survives a restart.** Accounts, rules, settings and members' sessions are on the volume; the newest requests are read back for the page.
- **One process.** The pool has one writer, so a restart is a few seconds without the server. Members' agents see failed requests for that moment and carry on.

## Backing it up

The volume and the vault key are the pool. Snapshot the volume with your cloud's tools, and keep a copy of the Secret that holds `LLMPOOL_VAULT_KEY`: one without the other is unreadable. For a copy you can restore anywhere, `llmpool export pool.age` on the server writes every account, encrypted with a passphrase.

## Limits worth knowing

- A member who is lent an access token can read it: they run the agent that uses it. Only the gateway keeps credentials from members.
- The ID token's signature is not verified separately: it is read only from the provider's token endpoint over verified TLS, which OpenID Connect allows.
- Rules on a member's machine are enforced by the member's llmpool. What a server controls for certain is what it hands out.

## Reporting a problem

Open an issue on GitHub for anything that is not sensitive. For a vulnerability, use GitHub's private vulnerability reporting on the repository.
