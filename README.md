# llmpool

A pool of LLM provider logins for coding agents. llmpool holds several
accounts, starts each agent session on the account a policy picks, and moves a
running session to another account when a policy says so, without restarting
the agent.

Works with Claude Code today, on macOS and Linux. The agent keeps its own login path and talks to
the provider directly: llmpool only decides which login sits in the profile the
agent reads. The one request it makes to a provider is renewing a login that
is sitting unused, so it still works when you need it (`LLMPOOL_NO_RENEW=1`
turns that off).

## Install

```bash
cargo build --release -p llmpool-cli
install -m 755 target/release/llmpool ~/.local/bin/llmpool   # `install`, not `cp`, when replacing a running copy
llmpool install claude           # hooks, status line and the /llmpool mod
llmpool install claude --shim    # optional: plain `claude` then starts through `llmpool run`
```

The window and tray app:

```bash
cargo install tauri-cli --version "^2" --locked
cd crates/llmpool-gui && cargo tauri build     # target/release/bundle/macos/llmpool.app
```

`llmpool uninstall claude` takes all of that back out and restores your
previous status line.

## Use

```bash
llmpool add claude --from-current --tag personal   # the login Claude Code has now
llmpool add claude --tag work                      # signs in to another account through your browser
llmpool login work@example.com                     # sign in again to an account whose login has ended
llmpool doctor                                     # what is missing, and the command that fixes it
llmpool list                                       # accounts, tags, 5-hour and 7-day usage
llmpool run                                        # start Claude Code on the account a policy picks
llmpool run --account work-1 -- --resume           # or name one; args after -- go to the agent
llmpool sessions
llmpool switch work-2                              # move this session to another account
```

Claude Code started on its own (a terminal without the shim, an editor
extension) is picked up too: it shows in `llmpool sessions`, reports usage,
and can be switched. Such sessions all run on the login in `~/.claude`, so they
switch together. A session started with `llmpool run` gets a login of its own,
which is what lets two accounts work side by side.

Inside Claude Code, `/llmpool` opens a pane with every account's usage and
reset times; press a number to switch to that account. The status line shows
the current account and its usage, once; the hint row under the prompt
carries the reminder `/llmpool`. A band appears above the prompt only when
something needs you: the account passes 80% (with a one-press switch to the
account with the most room), or its login is missing or has ended.

The app is the same in a window: Pool, Sessions, and a policy editor where
rules read as sentences and each has a Test button. Its tray menu lists
running sessions; each opens onto the accounts it can switch to. Closing the
window leaves it in the tray.

### Signing in

Signing in is Claude Code's own (`claude auth login`), run in a scratch
profile: your browser opens, and llmpool takes the login into the pool when
you finish. It never sees a password. The same thing is one press away in
`/llmpool` (`a`), in the app (**Sign in…**) and in the tray. If the page shows
a code instead of returning by itself, paste it where you started: the
terminal, the pane or the app.

Whatever is missing is named with its fix on every surface: a login Claude
Code uses that is not in the pool yet, an account whose login has ended,
hooks that are not installed.

### Switching quickly

| Where | How |
|---|---|
| Inside Claude Code | `/llmpool`, then a number key (`n` next, `a` add, `r` refresh); or `/llmpool next`, `/llmpool <account>`, `/llmpool add` |
| Any terminal | `llmpool switch --next`, or `llmpool switch <account>` |
| Anywhere, with the app running | ⌃⌥⌘N switches to the account with the most quota; ⌃⌥⌘L opens the window |
| Tray menu | each session opens onto the accounts it can switch to |
| In the app window | ⌘1–⌘3 sections, ⌘S save rules, ⌘R refresh |

The global key acts on the only running session, or on the login plain
`claude` sessions share; with several separately managed sessions it opens the
window so you can choose.

### Moving to another machine

```bash
llmpool export pool.age          # every account and its login, encrypted with a passphrase
llmpool import pool.age          # on the other machine
```

A login works on one machine at a time. After importing elsewhere, remove the
accounts here, or the two copies invalidate each other.

### Coming from claude-swap

```bash
llmpool import claude-swap
```

Brings over every account claude-swap holds on this machine, except the one
Claude Code is logged in as right now (add that with `--from-current`, which
has the newer copy). An imported login belongs to llmpool from then on:
claude-swap's own copy stops working the next time the login is used.

## Policies

Rules live in one TOML file (`llmpool policy path`), edited by hand or in the
GUI. The first rule that matches wins.

```toml
[[rule]]
name = "acme repos use acme seats"
on = "session_start"
when = { cwd = "~/work/acme/**" }
do = { assign = { tag = "acme", pick = "most_quota" } }

[[rule]]
name = "ask everywhere else"
on = "session_start"
do = { ask = {} }

[[rule]]
name = "swap before the limit"
on = { usage_above = { window = "5h", percent = 90 } }
do = { swap = { tag = "same" }, notify = true }

[[rule]]
name = "swap on rate limit"
on = "rate_limited"
do = { swap = {}, notify = true }
```

| | |
|---|---|
| Triggers | `session_start`, `usage_above`, `rate_limited`, `auth_failed`, `cwd_changed`, `schedule` |
| Conditions | `cwd`, `git_remote`, `model`, `account_tag`, `time` |
| Actions | `assign`, `swap`, `ask`, `notify`, `hold` |
| Pick | `most_quota`, `soonest_reset`, `round_robin` |

A session with no matching `session_start` rule runs on your own login,
untouched. Swaps have a cooldown (5 minutes) and, when driven by usage, need
the target to be 10 points better than the current account; both are in
`[settings]`.

`schedule` fires at a time of day: `on = { schedule = { at = "18:00", days = ["mon", "fri"] } }`.

When a login stops working, the account is marked as needing a new login and
skipped until you add it again.

To add a trigger: implement `Trigger` in
`crates/llmpool-core/src/policy/triggers.rs` and add one line to `all()`.

## How a swap works

Each managed session runs in its own Claude Code profile
(`CLAUDE_CONFIG_DIR`), with your settings, skills, plugins and history linked
in. To swap, llmpool takes Claude Code's own credential locks, saves the
outgoing login back to its vault (the agent may have rotated the token), and
writes the incoming login into the profile. Claude Code picks it up on its
own. The next prompt carries a note telling you and the model what happened.

A login's refresh token is single-use, so one account is never copied into two
profiles: sessions on the same account share a profile, and a swap moves all
of them together.

Secrets are kept in the OS keychain; account metadata in SQLite under
`~/Library/Application Support/llmpool` (or `$XDG_DATA_HOME/llmpool`).

## Provider terms

Subscription logins are licensed per person, and providers restrict what
third-party tools may do with them. llmpool is built for one person's own
accounts. Check the terms of each provider before using it, and before sharing
a pool with anyone.

## Layout

| Path | What |
|---|---|
| `crates/llmpool-core` | pool model, policy engine, account store, vault |
| `crates/llmpool-adapter-claude` | everything that knows Claude Code's files, locks and settings |
| `crates/llmpool-daemon` | session registry, swap logic, local HTTP API |
| `crates/llmpool-cli` | the `llmpool` binary |
| `crates/llmpool-gui` | Tauri window and tray |
| `claude-plugin/llmpool` | the Claude Code mod, embedded in the binary |
| `docs/superpowers/specs` | design |

## Development

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
claude plugin validate claude-plugin/llmpool && claude plugin test claude-plugin/llmpool
```

Tests never touch a real login: they run against temporary home directories
with a file vault. To try the binary the same way, set `LLMPOOL_HOME`, `HOME`,
`LLMPOOL_FILE_VAULT=1`, `LLMPOOL_NO_CLAUDE_KEYCHAIN=1` and `LLMPOOL_NO_NOTIFY=1`.

## Not built yet

Other agents and providers (Codex, Gemini CLI, Bedrock, API keys) and the team
server with inherited policies. Each is its own design; the adapter and
trigger traits are where they attach. Windows is not supported.

## Licence

MIT. See `LICENSE`.
