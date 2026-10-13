---
title: Overview
---
# One pool of LLM accounts for every coding agent you run

llmpool holds your logins and keys, starts each agent session on the account your rules pick, and moves on to another account when one fills up.
{: .lead}

![The /llmpool pane inside Claude Code]({{ '/assets/tui.png' | relative_url }})

```bash
curl -fsSL https://raw.githubusercontent.com/shakedaskayo/llmpool/main/scripts/install.sh | sh
```

## What it does

- **In your agent.** In Claude Code, `/llmpool` shows this session's usage and the rest of the pool, and switches account with one key. The session keeps running. [In Claude Code]({{ '/claude-code/' | relative_url }})
- **Any agent, any provider.** Codex CLI, OpenCode, Gemini CLI, Grok, Aider or your own script start through `llmpool exec`, on a subscription login or a key. [Any agent]({{ '/agents/' | relative_url }})
- **Rules.** "Work repos use work accounts", "move before the limit", "prefer my own account while it has room". [Rules]({{ '/rules/' | relative_url }})
- **For a team.** One server holds the pool. Members sign in with Google or any OpenID Connect provider, or with an API key. [Team server]({{ '/team/' | relative_url }})
- **As a gateway.** The server can be what every request goes through, so no login or key leaves it and switching happens on the server. [Gateway]({{ '/gateway/' | relative_url }})

![Accounts in the app]({{ '/assets/accounts.png' | relative_url }})

## What each agent gets

| | Accounts | Chosen | Switched while running | Usage read |
|---|---|---|---|---|
| Claude Code | its own logins | by rules, at start and after | yes | yes |
| Codex CLI, OpenCode | their own logins, or keys | by rules, at start | no | no |
| Gemini CLI, Grok, Aider, anything else | keys | by rules, at start | no | no |
| Anything, through a team's gateway | the team's logins and keys | by the server, per request | yes, on the server | for Claude subscriptions |

## Before you share a pool

Subscription logins are licensed per person, and providers restrict what tools may do with them. llmpool is built first for one person's own accounts. A team server lends a subscription login to one member at a time unless you tell it otherwise. Check each provider's terms before sharing.
