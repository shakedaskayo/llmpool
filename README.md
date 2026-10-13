# llmpool

A pool of LLM provider logins for coding agents. llmpool holds several
accounts, starts each agent session on the account a policy picks, and moves a
running session to another account when a policy says so, without restarting
the agent.

Works with Claude Code today. The agent keeps its own login path and talks to
the provider directly: llmpool only decides which login sits in the profile the
agent reads. It makes no requests to any provider.

## Install

```bash
cargo build --release            # builds llmpool (CLI + daemon) and llmpool-gui
cp target/release/llmpool ~/.local/bin/
llmpool install claude --shim    # hooks, status line, the /llmpool mod, and a `claude` shell function
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

With `--shim`, plain `claude` goes through `llmpool run`.

Inside Claude Code, `/llmpool` opens a pane with every account's usage and a
Switch button per account. The status line shows the current account and its
usage; a band appears above the prompt when it passes 80%.

`llmpool-gui` is the same thing in a window: Pool, Sessions, and a policy
editor with a Test button per rule.

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
| Triggers | `session_start`, `usage_above`, `rate_limited`, `auth_failed`, `cwd_changed` |
| Conditions | `cwd`, `git_remote`, `model`, `account_tag`, `time` |
| Actions | `assign`, `swap`, `ask`, `notify`, `hold` |
| Pick | `most_quota`, `soonest_reset`, `round_robin` |

A session with no matching `session_start` rule runs on your own login,
untouched. Swaps have a cooldown (5 minutes) and, when driven by usage, need
the target to be 10 points better than the current account; both are in
`[settings]`.

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
cargo test --workspace
claude plugin test claude-plugin/llmpool
```

## Not built yet

Other agents and providers (Codex, Gemini CLI, Bedrock, API keys), the team
server, usage polling for idle accounts, the `schedule` trigger, encrypted
export/import, and import from claude-swap.
