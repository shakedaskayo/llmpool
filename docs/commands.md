---
title: Commands
permalink: /commands/
---
# Commands

`llmpool <command> --help` has the details of each.

## Accounts

| | |
|---|---|
| `llmpool add claude [--from-current]` | Sign in to a Claude account, or take the login Claude Code has now |
| `llmpool add login <agent> [--new]` | Take the login another agent has; `--new` signs it in to another account first |
| `llmpool add key <provider> [--env NAME]` | A key for any provider |
| `llmpool list` | Accounts and their usage |
| `llmpool tag` / `untag` / `enable` / `disable` / `remove` | Manage an account |
| `llmpool login <account>` | Sign in again to an account whose login has ended |
| `llmpool export <file>` / `import <file>` | Move a pool between machines, encrypted with a passphrase |
| `llmpool import claude-swap` | Bring over what claude-swap holds |

## Sessions

| | |
|---|---|
| `llmpool run [--account A] [--tag T] -- <args>` | Start Claude Code on the account a rule picks |
| `llmpool exec [--account A] [--provider P] -- <command>` | Start any agent on a key or login from the pool |
| `llmpool sessions` | What is running |
| `llmpool switch <account>` / `--next` | Move a running Claude Code session |
| `llmpool status` | This session's account and usage |
| `llmpool repair` | Write a session's login afresh without changing account |

## Rules and choosing

| | |
|---|---|
| `llmpool policy path` / `check` / `dry-run` | Find, validate and test rules |
| `llmpool off` / `llmpool on` | Agents on your own login with no switching, or back on the pool |
| `llmpool prefer-own on\|off` | Your own account first while it has room |
| `llmpool advisor [on\|off\|key\|ask]` | The optional decision model |

## Setup

| | |
|---|---|
| `llmpool doctor` | What is in place and how to fix what is not |
| `llmpool agents` | The agents llmpool can start and what it found of each |
| `llmpool setup [--undo]` | macOS: command, Claude Code, current login, start at login |
| `llmpool install claude [--shim]` / `install agents` | Connect Claude Code; make plain `codex`, `opencode`, ... go through the pool |
| `llmpool uninstall claude` / `uninstall agents` | Take those back out |

## Team

| | |
|---|---|
| `llmpool team join <address> [--key]` | Join in one go: connect, have new sessions use the pool, make plain `claude` start through it |
| `llmpool team connect <address> [--key]` | Only connect |
| `llmpool team share <account>` | Put one of your accounts into the team's pool (administrators) |
| `llmpool team` / `team sync` / `team disconnect` | The link, a sync now, leaving |
| `llmpool serve --listen <address>` | Serve this machine's pool to a team |
