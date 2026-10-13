# llmpool

A pool of LLM provider credentials for coding agents. llmpool holds several
logins and keys, assigns one to each agent session, and swaps the credential of
a running session when a policy says so.

Status: foundation only. The policy engine works and can be tested from the
command line. The daemon, Claude Code integration and GUI are not built yet.
See `docs/superpowers/specs/` for the design.

## Try the policy engine

```bash
cargo build --release
./target/release/llmpool policy check policies.toml
./target/release/llmpool policy dry-run policies.toml scenario.json
```

A policy file is an ordered list of rules; the first rule that matches wins:

```toml
[[rule]]
name = "acme repos use acme seats"
on = "session_start"
when = { cwd = "~/work/acme/**" }
do = { assign = { tag = "acme", pick = "most_quota" } }

[[rule]]
name = "swap before the limit"
on = { usage_above = { window = "5h", percent = 90 } }
do = { swap = { tag = "same" }, notify = true }
```

Triggers: `session_start`, `usage_above`, `rate_limited`, `auth_failed`,
`cwd_changed`. Conditions: `cwd`, `git_remote`, `model`, `account_tag`, `time`.
Actions: `assign`, `swap`, `ask`, `notify`, `hold`. Pick strategies:
`most_quota`, `soonest_reset`, `round_robin`.

## Development

```bash
cargo test --workspace
```
