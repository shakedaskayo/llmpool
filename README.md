# llmpool

A pool of LLM provider logins for coding agents. llmpool holds several
accounts, starts each agent session on the account a policy picks, and moves a
running session to another account when a policy says so, without restarting
the agent.

Works with Claude Code today, on macOS and Linux. The agent keeps its own login path and talks to
the provider directly: llmpool only decides which login sits in the profile the
agent reads. It makes no requests to any provider.

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
llmpool add claude --tag work                      # opens Claude Code in a scratch profile to log in
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
the current account and its usage. When the account passes 80% a band appears
above the prompt with a one-press switch to the account with the most room.

The app is the same in a window: Pool, Sessions, and a policy editor where
rules read as sentences and each has a Test button. Its tray menu lists
running sessions; each opens onto the accounts it can switch to. Closing the
window leaves it in the tray.

### Moving to another machine

```bash
llmpool export pool.age          # every account and its login, encrypted with a passphrase
llmpool import pool.age          # on the other machine
```

A login works on one machine at a time. After importing elsewhere, remove the
accounts here, or the two copies invalidate each other.

### Coming from claude-swap

For each account: `cswap switch N`, then `llmpool add claude --from-current`.

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
