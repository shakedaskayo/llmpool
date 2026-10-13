# llmpool core design

Date: 2026-10-13
Status: implemented; see “As built” at the end for where the code differs

## Purpose

llmpool is a local pool of LLM provider credentials for coding agents. It holds
several logins and keys (subscription OAuth logins, API keys, cloud
credentials), assigns one to each agent session, and swaps the credential of a
running session when a policy says so, without restarting the agent.

It is for a developer who has more than one account or provider and wants the
right one used in the right place, and later for a team that manages a shared
set of credentials.

Success for this spec:

- A user adds two or more Claude accounts and sees live usage for each.
- Starting Claude Code in a directory picks the account a policy names, or asks.
- When the active account nears its limit, the session moves to another account
  and both the user and the model are told, with no restart.
- Policies are edited in a GUI without touching a file.

## Scope

In this spec:

1. Daemon, vault, pool, session registry.
2. Claude Code adapter (macOS tested; code kept portable).
3. Policy engine with pluggable triggers.
4. CLI.
5. Tauri GUI: tray, Pool, Sessions, Policies.
6. Installer for Claude Code hooks and statusline.
7. Import from claude-swap.

Out of this spec, each a later spec:

- Other adapters: API key and Bedrock, Codex, OpenCode, Gemini CLI, Copilot CLI.
- Local proxy adapter (opt-in, API-key and cloud credentials only).
- Team server: sync, roles, audit, checkout leases.
- Windows and Linux packaging.
- Packaging as a Claude Code plugin.

## Requirements recorded for the team server spec

Not designed here; captured so the later spec starts from them.

- Policies can be inherited from the server. Inherited rules are evaluated
  before local ones and cannot be edited or disabled on the client.
- Accounts provided by the server cannot be edited, removed or exported on the
  client.
- A client on a user-owned machine can always bypass a local daemon, so
  inherited policy is enforced only where the server controls what the client
  receives: the server hands out short-lived credentials (leases) rather than
  long-lived ones, and stops handing them out to a client that is out of
  policy.
- A credential the agent reads directly is readable by the user who runs the
  agent. Only credentials served through a helper command or a proxy can be
  kept from the user, and of those only short-lived ones limit the damage.

## Approach

Native-path broker. The agent keeps its own login path and talks to the
provider directly. llmpool only decides which credential sits in the store the
agent reads, and keeps that credential fresh. llmpool makes no model requests.
The only provider calls it makes with a subscription token are token refresh
and the usage read.

A proxy was rejected as the default because it puts llmpool between the agent
and the provider, disables Claude Code features that depend on the first-party
host, and requires tracking every provider's wire format. The adapter trait is
shaped so a proxy adapter can be added later.

## Provider terms

Provider terms restrict third-party handling of subscription credentials and
sharing one login between people. This shapes the design:

- llmpool never implements a provider login flow. Accounts are added by running
  the agent's own login and capturing the result.
- Local mode manages one person's own accounts.
- The team server spec will default to API keys, cloud credentials and
  one-seat-per-person assignment.
- The GUI shows a notice when a subscription login is added, stating that the
  user is responsible for compliance with that provider's terms.

## Architecture

Cargo workspace:

| Crate | Responsibility | Depends on |
|---|---|---|
| `llmpool-core` | Pool model, vault, policy engine, adapter and trigger traits | none of the others |
| `llmpool-adapter-claude` | Everything that knows Claude Code's files, Keychain items, locks, endpoints | core |
| `llmpool-daemon` | Process wiring: HTTP API, scheduler, session registry | core, adapters |
| `llmpool-cli` | `llmpool` binary, thin client of the daemon API | core (types only) |
| `llmpool-gui` | Tauri 2 app, thin client; can host the daemon in-process | daemon |

One daemon per user. If the GUI starts and no daemon is running, it runs the
daemon in-process; if the CLI needs a daemon and none runs, it starts one
detached. A lock file in the data dir guarantees a single daemon.

Data dir: `~/Library/Application Support/llmpool` on macOS, XDG data dir
elsewhere. Contents: `llmpool.db` (SQLite), `policies.toml`, `sessions/`,
`daemon.lock`, `daemon.token`.

### Daemon API

HTTP with JSON bodies, served on a Unix socket for CLI and GUI and on
`127.0.0.1:<port>` for agent `http` hooks. Every request carries the bearer
token from `daemon.token` (file mode 0600). The loopback listener accepts only
the hook and statusline routes.

Routes:

- `GET /accounts`, `POST /accounts/capture`, `PATCH /accounts/{id}`,
  `DELETE /accounts/{id}`
- `GET /sessions`, `POST /sessions` (register and assign),
  `POST /sessions/{id}/swap`, `DELETE /sessions/{id}`
- `GET /policies`, `PUT /policies`, `POST /policies/dry-run`
- `POST /hooks/claude/{event}` (loopback)
- `POST /statusline/claude` (loopback)
- `GET /events` (server-sent events for the GUI)

## Pool and vault

Account fields: `id`, `provider`, `kind` (`oauth`, `api_key`, `cloud`),
`label`, `identity` (email and organization as the provider reports them),
`tags` (set of strings), `enabled`, `health` (`ok`, `relogin_required`,
`quarantined`), `usage` (last reading with timestamp), `login_expires_at`.

Secrets are stored in the OS keychain through the `keyring` crate under service
`llmpool`, one item per account. Metadata lives in SQLite. Secrets never enter
SQLite, logs, or API responses.

Export and import use a single file encrypted with a passphrase (`age` crate).
There is no plaintext export.

### Single refresher and leases

Claude and Codex refresh tokens are single-use: a second copy of a login that
refreshes independently invalidates the first. Two rules follow.

1. For every account in the pool, the daemon is the only writer of its stored
   secret. When a live agent store holds that account, the agent may refresh it
   itself; the daemon reads the rotated token back from that store, under the
   agent's lock, before any other use.
2. An OAuth account is leased to at most one credential store at a time. A
   second session assigned the same account is attached to the same store, so
   both read one credential. It never receives a copy.

Refresh by the daemon happens only for accounts with no live store, shortly
before expiry, so an idle account is ready to activate.

A refresh that fails with `invalid_grant` sets `health = relogin_required` and
raises a notification. Network failures keep the last state and back off.

### Adding and re-adding accounts

`llmpool add claude` launches Claude Code's own `/login` in a scratch profile
(a temporary config dir and secure-storage dir), waits for a credential to
appear, reads identity, stores the secret, and removes the scratch profile
without logging out, because logout revokes the refresh token.

If the identity matches an existing account, its secret is replaced and health
returns to `ok`. This is the re-login path; the GUI's "Sign in again" button
runs the same flow.

`llmpool add claude --from-current` captures the default login already present
on the machine.

`llmpool import claude-swap` reads the accounts claude-swap has stored and adds
them. Accounts whose tokens are dead are imported with
`health = relogin_required`.

## Sessions and assignment

A session is one agent process launched through llmpool. Fields: `id`,
`agent`, `pid`, `cwd`, `git_remote`, `account_id`, `store_id`, `started_at`,
`agent_session_id` (filled by the first hook call).

Launch path: `llmpool run [--account <label>] [--tag <tag>] [-- <agent args>]`.
`llmpool install claude --shim` also adds a shell function so plain `claude`
goes through `llmpool run`.

Steps at launch:

1. CLI sends cwd, git remote and any explicit flag to `POST /sessions`.
2. Daemon evaluates `session_start` rules. Outcomes:
   - `assign`: an account is chosen.
   - `ask`: the daemon returns candidates; the CLI shows a picker in the
     terminal. If the GUI is open and the launch has no TTY, the GUI shows the
     picker instead.
   - no rule matches: the agent starts on its default login and the session is
     registered as unmanaged. It is visible in the GUI and can be assigned
     later only by restart.
3. For a managed session the daemon prepares a store: a directory under
   `sessions/<store_id>` used as `CLAUDE_SECURESTORAGE_CONFIG_DIR`. The agent's
   config dir stays the user's normal one, so settings, skills, MCP servers and
   history are shared.
4. Daemon writes the assigned credential into that store and returns the
   environment to set. CLI execs the agent with it.
5. On agent exit the CLI calls `DELETE /sessions/{id}`. The daemon reads back
   the possibly rotated credential, updates the vault, and removes the store
   if no other session is attached.

Explicit `--account` or `--tag` skips rule evaluation.

Per-directory binding is not a separate feature. It is a `session_start` rule
with a `cwd` condition.

## Signals

Sources the daemon learns from:

| Source | Carries | Used for |
|---|---|---|
| Statusline wrapper | `rate_limits` five-hour and seven-day percent and reset times, model, cwd, agent session id | Usage of active accounts, free |
| `SessionStart` hook | agent session id, cwd, source | Bind agent session to llmpool session |
| `UserPromptSubmit` hook | agent session id | Deliver pending notices |
| `StopFailure` hook | error kind (`rate_limit`, `authentication_failed`, `billing_error`) | `rate_limited`, `auth_failed` triggers |
| `CwdChanged` hook | new cwd | `cwd_changed` trigger |
| `SessionEnd` hook | agent session id | Cleanup fallback if the CLI died |
| Usage poll | Usage of accounts with no live session | Ranking candidates |

The statusline wrapper is `llmpool statusline`. It reads the JSON on stdin,
posts it to the daemon with a 200 ms timeout, then runs the user's previous
statusline command with the same stdin and prints its output. If there was no
previous command it prints a short llmpool line. A daemon that is down must
never break the statusline.

Hooks are `http` hooks pointing at the loopback listener. A daemon that is down
makes the hook fail non-blocking; the agent continues.

Usage polling of idle accounts is adaptive: at most two accounts per tick,
candidates near a decision more often, exhausted accounts about every ten
minutes, exponential backoff on 429.

## Policy engine

A policy file is an ordered list of rules. For each event the engine walks the
rules whose trigger matches, in order, and runs the action of the first rule
whose conditions hold.

```toml
[[rule]]
name = "acme repos use acme seats"
enabled = true
on = "session_start"
when = { cwd = "~/work/acme/**" }
do = { assign = { tag = "acme", pick = "most_quota" } }

[[rule]]
name = "ask everywhere else"
on = "session_start"
do = { ask = { tag = "personal" } }

[[rule]]
name = "swap before the limit"
on = { usage_above = { window = "5h", percent = 90 } }
do = { swap = { tag = "same", pick = "most_quota" }, notify = true }

[[rule]]
name = "swap on rate limit"
on = "rate_limited"
do = { swap = { pick = "most_quota" }, notify = true }
```

### Triggers

A trigger is a type implementing:

```rust
pub trait Trigger: Send + Sync {
    /// Name used in the `on` field.
    fn kind(&self) -> &'static str;
    /// Does this event fire this trigger, given the rule's parameters?
    fn matches(&self, params: &toml::Value, event: &Event, ctx: &Context) -> bool;
    /// Parameter schema, used by the GUI to render the editor.
    fn schema(&self) -> ParamSchema;
}
```

Triggers are registered in one table in `llmpool-core`. Adding one means
writing a struct and one registration line; the GUI picks up its editor from
`schema()`.

Triggers in this spec: `session_start`, `usage_above` (`window`: `5h` or `7d`
or a model name, `percent`), `rate_limited`, `auth_failed`, `cwd_changed`,
`schedule` (cron expression).

### Conditions

All listed conditions must hold. `cwd` (glob), `git_remote` (glob),
`account_tag` (the session's current account has this tag), `model` (glob),
`time` (weekday and hour range). Unknown condition keys are a validation error.

### Actions

- `assign { account | tag, pick }`: only valid on `session_start`.
- `swap { account | tag, pick }`: change the credential of a running session.
  `tag = "same"` means the tags of the current account.
- `ask { tag }`: let the user choose among matching accounts.
- `notify`: tell the user and model without changing anything.
- `hold`: do nothing and stop evaluating. Used to exempt a directory.

`pick` selectors: `most_quota`, `soonest_reset`, `round_robin`. Candidates are
always filtered to enabled accounts with `health = ok` that are not above the
rule's own threshold.

Swap guards, global settings with defaults: cooldown of 5 minutes per session,
and a margin of 10 percentage points that a candidate must beat the current
account by for a threshold-driven swap. `rate_limited` and `auth_failed` swaps
ignore the margin. When no candidate qualifies the daemon emits
`all_exhausted`, notifies once, and rechecks at the earliest reset time.

`POST /policies/dry-run` takes a synthetic event and returns which rule would
fire and which account would be chosen. The GUI uses it for the preview.

Validation runs on every write. An invalid file is rejected and the previous
one stays active.

## Swap and notification

Swap of a managed Claude session:

1. Take Claude Code's credential lock for the session's store.
2. Read the store's current credential back into the vault for the outgoing
   account, since the agent may have rotated it.
3. Refresh the incoming account's token if it is near expiry.
4. Write the incoming credential and the matching identity block to the store.
5. Release the lock, move the lease, record a `swap` event.

A swap acts on the store, so every session attached to that store moves
together and each receives the notice. A session that should move alone has to
be restarted onto its own account.

An `ask` action fired by a running session is shown as a GUI picker. If the
GUI is not running it degrades to `notify`, naming the candidates and the
`llmpool switch` command.

If step 3 or 4 fails, the outgoing credential is left in place and the swap is
reported as failed. The store is never left empty.

Claude Code reads the Keychain through a cache of about thirty seconds, so a
swap takes effect within that time. The session keeps working on the old
credential until then.

Notification: the daemon queues a notice for the session. The next
`UserPromptSubmit` hook response carries it as `systemMessage` for the user and
`additionalContext` for the model, for example: `llmpool: this session moved
from work-1 to work-2 because the 5-hour window reached 91%.` An OS
notification is sent at swap time. Notices that are never collected expire
when the session ends.

The first message after a swap rebuilds the prompt cache on the new account and
costs more than usual. The notice says so.

## CLI

```
llmpool add claude [--from-current] [--label L] [--tag T]...
llmpool list [--json]
llmpool tag <account> <tag>... | untag <account> <tag>...
llmpool enable | disable | remove <account>
llmpool run [--account A | --tag T] [-- <agent args>]
llmpool sessions [--json]
llmpool switch [<session>] [<account>]
llmpool policy check | edit | dry-run <event-json>
llmpool install claude [--shim] | uninstall claude
llmpool import claude-swap
llmpool export <file> | import <file>
llmpool daemon [--foreground] | status | stop
```

`--json` output carries a `schemaVersion` field.

## GUI

Tauri 2, web frontend in React and TypeScript. Plain and dense, no scroll
animations, light and dark following the system.

Tray menu: one row per live session with account label and five-hour bar;
click a row to choose another account; "Open llmpool"; "Pause all policies".

Window with three tabs:

- **Pool**: table of accounts with label, provider, tags as chips, five-hour
  and seven-day bars with reset time, health. Row actions: edit tags, disable,
  sign in again, remove. "Add account" button.
- **Sessions**: live sessions with agent, directory, account, started time,
  last event. Row action: switch account. Below it, a log of recent events.
- **Policies**: ordered list of rules. Each rule is one row read as a sentence:
  *When* [trigger and its parameters] *if* [conditions] *then* [action and
  selector]. Each bracket is a dropdown or small inline field generated from
  the trigger, condition and action schemas. Drag handle to reorder, toggle to
  enable, a "Test" button that runs dry-run against a chosen session and shows
  the result inline. A "View as TOML" toggle shows the file. Saving writes
  `policies.toml` through the API, so file and GUI are always the same thing.

Pickers for `ask` appear as a small always-on-top window when the launch has no
terminal.

## Installer

`llmpool install claude`:

1. Back up the user's `settings.json` next to it with a timestamp.
2. Add `http` hooks for the events listed under Signals, pointing at the
   loopback listener, tagged so they can be found again.
3. Record the existing `statusLine` command in llmpool's settings and replace
   it with `llmpool statusline`.
4. With `--shim`, append a marked block to the user's shell rc file.

`llmpool uninstall claude` removes exactly what was added and restores the
previous statusline command. Running install twice changes nothing.

## Error handling

| Situation | Behaviour |
|---|---|
| Daemon down at `llmpool run` | CLI starts it; if that fails, run the agent on its default login and print one warning |
| Daemon down during a session | Hooks and statusline fail open; session continues on its current credential |
| Refresh fails, network | Keep state, back off, retry |
| Refresh fails, `invalid_grant` | `relogin_required`, notify, exclude from candidates |
| Swap target fails to activate | Keep outgoing credential, report, try the next candidate once |
| No candidate | `all_exhausted` notice once, recheck at earliest reset |
| Invalid policy file | Reject write, keep previous rules, show the error with line number |
| Store lock held too long | Abort the swap after 5 seconds, retry at the next trigger |
| Crashed session left a store | On daemon start, stores whose pid is dead are read back and removed |

## Testing

- Policy engine, selectors, guards: unit tests over synthetic events and pools.
  This is pure logic and carries most of the coverage.
- Vault: tests against an in-memory keychain backend.
- Claude adapter: tests against temporary config and storage dirs with a file
  backend and a fake token endpoint. Tests must refuse to run if they resolve
  to the real store.
- Installer: golden-file tests for install, repeat install and uninstall on
  sample settings files.
- Daemon API: integration tests over the Unix socket with a fake adapter.
- Manual checklist on macOS for the parts that need a real account: capture,
  launch with assignment, live swap picked up within a minute, notice
  delivered, re-login.

## Risks and open items

- **Live reload of a per-session Keychain item is not yet confirmed.** The
  default item is known to reload within about thirty seconds. The first
  implementation task is a test of the per-session item. If it does not
  reload, managed sessions on macOS fall back to swapping by restart with
  `--resume`, and the notice tells the user to do so.
- Claude Code's storage, lock and endpoint details are not a public contract
  and can change between versions. All of it is confined to
  `llmpool-adapter-claude`, which checks the installed version and refuses to
  write to a store it does not recognise.
- `rate_limits` appears in statusline input only for subscription plans and
  only after the first response. Until then the daemon uses its last polled
  reading.
- Provider terms, as described above.

## As built

Where the implementation differs from the design above, and why.

| Design | As built | Why |
|---|---|---|
| Session keeps the user's config dir; only the credential store is separate (`CLAUDE_SECURESTORAGE_CONFIG_DIR`) | Each managed session gets its own `CLAUDE_CONFIG_DIR` with the user's settings, skills, plugins, history and markdown linked in, and user-scope MCP servers copied | Identity lives in the config dir's `.claude.json`; a separate credential store alone would pair one account's token with another's identity |
| Sessions exist only when started by `llmpool run` | Claude Code started on its own is adopted on its first hook, in the user's own profile; all such sessions share one login and switch together | Lets the status line, the mod and auto-switching work without changing how Claude Code is launched |
| Daemon refreshes tokens of idle accounts and polls their usage | The daemon makes no provider requests at all | Claude Code refreshes its own token on use; avoiding provider calls keeps llmpool out of the request path. Cost: an idle account shows its last reading until its window resets |
| `http` hooks | `command` hooks (`llmpool hook claude <event>`) | The launcher's session id reaches a command through its environment; a command can also fail silently, which a hook must |
| Unix socket for CLI and GUI, loopback for hooks | Loopback TCP only, one bearer token, file mode 0600 | One transport to secure and test |
| `schedule` takes a cron expression | `schedule = { at = "HH:MM", days = [...] }` | Covers switching at a time of day without a cron parser; the engine is ticked once a minute |
| GUI in React and TypeScript, live via server-sent events, tray with per-session switching | Plain HTML, CSS and JavaScript with no build step, polling every three seconds; tray lists sessions with a submenu of accounts to switch to | Fewer moving parts for the same screens |
| `ask` on a running session opens a GUI picker | It queues a notice naming the candidates; the choice is made in `/llmpool`, the GUI or `llmpool switch` | One place to choose from, already present in every surface |
| Account health `quarantined` | Unused; a login that fails is `relogin_required` | One state was enough |
| Windows | Not supported: process, lock and permission handling use Unix calls | macOS and Linux first |

Verified against real Claude Code on macOS: install, capture of the current
login, a managed session, adoption of plain sessions, hook delivery and usage
readings. Not yet verified against real Claude Code: a swap between two real
accounts.
