---
title: In Claude Code
permalink: /claude-code/
---
# In Claude Code

Start `claude` the way you always do. llmpool tells you at the start which account the session runs on, and the status line shows how full it is.

![The /llmpool pane]({{ '/assets/tui.png' | relative_url }})

## The pane

`/llmpool` opens it.

| Key | Does |
|---|---|
| `1`–`9` | Switch this session to that account |
| `n` | Switch to the account with the most room |
| `a` | Sign in to another account |
| `r` | Refresh |
| `esc` | Close |

**This session** shows both windows with a mark for where even use would be by now, when each resets, and whether the current pace lasts until then. **Pool** lists the other Claude logins; one that is live in another session is marked and cannot be taken. A row above the prompt offers the best accounts to switch to when the pane is closed.

## Without the pane

```
/llmpool next              switch to the account with the most room
/llmpool work-2            switch to an account by name
/llmpool add               sign in to another account
/llmpool team <address>    join a team's pool
```

## What a switch is

llmpool writes the other account's login into the profile the session reads, under the same locks Claude Code uses. The session notices within about a minute and carries on; the first message on the new account rebuilds the prompt cache. Sessions started as plain `claude` share one login and move together. `llmpool run` gives a session a profile of its own, so it can be on a different account from the others.

## When something is missing

A band above the prompt appears only when something needs you: the account is nearly full, its login has ended, or the login Claude Code uses is not in the pool. The band carries the button that fixes it.
