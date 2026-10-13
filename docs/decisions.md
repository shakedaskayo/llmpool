---
title: Decision model
permalink: /decisions/
---
# A decision model for heavy use

Rules handle the plain cases: this repo uses that account, move at 90%. A team that leans hard on a handful of accounts hits the cases in between. Three people are deep in long sessions on the same account. One account is half used with four hours to go, another is nearly full but resets in ten minutes. Is it worth moving a session now and paying for a cold prompt cache, or will it last?
{: .lead}

llmpool can put those questions to a decision model. It is optional, it works inside your rules, and it is off until you turn it on.

## What it is asked

When a session's account passes half of a window, and when a new session needs an account, llmpool describes the pool and asks:

- **Should this session move?** A yes-or-no with a probability.
- **Where to?** A choice among the accounts it could move to, or staying.
- **How urgent is it?** Can wait, within minutes, or now.

The picture it gets, for each account: how much of each window is used, when it resets, how many sessions are running on it **including your teammates'**, and whether it will last to the reset at the current pace. Accounts are named by letter. No address, no token, no prompt and no code is sent.

For a new session the question is simpler: which account should this start on, so that it can work longest without anyone being squeezed?

## What it may do

- It acts only when it is at least as sure as you require (70% by default).
- It can only choose accounts your rules would allow: usable, not held by someone else, within the cooldown.
- Your own account still comes first if you have said so.
- It has a daily budget of questions (200 by default), and asking never holds a session up: the answer is applied when it arrives.
- When it is off, unreachable or unsure, your rules decide alone, exactly as before.

Every answer is shown where you would look: on the session in the app and in the `/llmpool` pane, with how sure it was and whether it acted.

## Why it helps a team

Rules look at one account at a time. The model is given the whole pool at once, with everyone's load on it. That is what lets it spread heavy use across accounts before any one of them is exhausted, leave a session where it is when moving would cost more than it saves, and send a new session to the account that will carry it longest rather than the one that merely looks emptiest right now.

## Turning it on

```bash
llmpool advisor key        # store the model's API key in your keychain (typed, never an argument)
llmpool advisor on
llmpool advisor            # status: on or off, how sure it must be, questions used today
llmpool advisor ask        # ask about this session now, and see the answer
```

Or in the app: **Settings → Advisor**. To let a rule hand the choice to the model:

```toml
[[rule]]
name = "let the model place new sessions"
on = "session_start"
do = { assign = { pick = "advisor" } }
```

The model used today is [Jev](https://jevai.info), a model built for fast structured decisions rather than text. One question takes about half a second.

## Where it applies

- **On each member's machine**: which account a session starts on, and when a running Claude Code session should move.
- **On a team's gateway**: where each member's requests go next, once their account is past half of a window. See [Gateway]({{ '/gateway/' | relative_url }}). Its decisions are listed on the server's Requests page.
