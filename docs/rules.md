---
title: Rules
permalink: /rules/
---
# Rules

Rules live in one TOML file (`llmpool policy path`), edited by hand or in the app. They are tried top to bottom; the first that matches decides.

![The rules editor]({{ '/assets/rules.png' | relative_url }})

```toml
[[rule]]
name = "work repos use work accounts"
on = "session_start"
when = { cwd = "~/work/**" }
do = { assign = { tag = "work", pick = "least_busy" } }

[[rule]]
name = "move before the limit"
on = { usage_above = { window = "5h", percent = 90 } }
do = { swap = { tag = "same" }, notify = true }

[[rule]]
name = "move when refused"
on = "rate_limited"
do = { swap = {}, notify = true }
```

| | |
|---|---|
| When | `session_start`, `usage_above`, `rate_limited`, `auth_failed`, `cwd_changed`, `schedule`, `pressure`, `own_available` |
| Only if | `cwd`, `git_remote`, `model`, `account_tag`, `time` |
| Then | `assign`, `swap`, `ask`, `notify`, `hold` |
| Which account | `most_quota`, `least_busy`, `soonest_reset`, `round_robin`, `advisor` |

A session with no matching `session_start` rule runs on your own login, untouched. Switches have a cooldown (5 minutes) and, when driven by usage, need the target to be 10 points emptier; both are in `[settings]`.

## Your own account first

`llmpool prefer-own on` (or the switch in Settings) uses an account tagged `own` whenever it has room, and the rest of the pool only when it does not. The login Claude Code had when you set llmpool up is tagged `own` for you.

## Pressure

`pressure` fires when several sessions on one account are on course to fill a window before it resets, and moves one of them:

```toml
[[rule]]
name = "spread out under pressure"
on = { pressure = { window = "5h", full_within_minutes = 45, min_sessions = 2 } }
do = { swap = { pick = "least_busy" } }
```

## Try a rule before trusting it

`llmpool policy dry-run rules.toml scenario.json` says which rule would fire for a described situation; the app's editor has the same test on every rule.

## The decision model

Optional. A decision model can be given the whole pool and asked when to move a session and where to start a new one. See [Decision model]({{ '/decisions/' | relative_url }}).
