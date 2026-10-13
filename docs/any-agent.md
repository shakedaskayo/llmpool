---
title: Any agent
permalink: /agents/
---
# Any agent, any provider

Claude Code is the agent llmpool knows best. Every other agent works through one command, `llmpool exec`, which starts it on the account your rules pick.

```bash
llmpool agents                                   # what llmpool found on this machine
llmpool add key openai --tag work                # a key for any provider
llmpool add key together --env TOGETHER_API_KEY  # a provider llmpool has not heard of
llmpool add login codex                          # the login Codex CLI (or OpenCode) has now
llmpool add login codex --new                    # sign it in to a second account first
llmpool exec -- codex                            # a codex login if the pool has one, else an OpenAI key
llmpool exec --provider xai -- my-script.sh      # anything that reads a key from its environment
llmpool install agents                           # plain `codex`, `opencode`, ... go through the pool
```

![Agents]({{ '/assets/agents.png' | relative_url }})

## Keys

A key reaches the agent as an environment variable. llmpool knows the variables of Anthropic, OpenAI, xAI, Gemini, OpenRouter, Groq, Mistral, DeepSeek, Bedrock and Azure OpenAI; for anything else, name them with `--env`.

An account may only set variables that look like a credential (`*_API_KEY`, `*_TOKEN`, `*_BASE_URL`, and a few more). Neither a file you import nor a team's server can set `PATH` or `LD_PRELOAD` for your agent.

## An agent's own login

Codex CLI and OpenCode sign in themselves and keep that login in their own files. llmpool takes a copy. A session on the account the agent is already logged in to runs where it always does. A session on another account gets a directory with that login and links to everything else the agent keeps, so settings and history stay shared. When the session ends the login is read back, because agents renew their tokens in place.

## An agent llmpool does not know

A few lines in `harnesses.toml`, in llmpool's data directory:

```toml
[[harness]]
name = "mycli"
title = "My CLI"
command = "mycli"
provider = "openrouter"        # the key it reads, if it reads one

[harness.login]                # only if it keeps a login in files
home_env = "MYCLI_HOME"
base = ".mycli"
files = ["auth.json"]
login_args = ["login"]
```

## What is different from Claude Code

These agents read their account when they start. llmpool does not change it under a running one, and providers do not report its usage to llmpool. Through a team's [gateway]({{ '/gateway/' | relative_url }}) the server switches accounts per request, for any agent.
