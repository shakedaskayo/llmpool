---
title: Gateway
permalink: /gateway/
---
# The server as a gateway

With the gateway on, members' agents send their requests to the server, and the server forwards each one to the provider on an account from the pool. **No login or key leaves the server.** A member's agent holds only the member's own session with the server.

Turn it on with `LLMPOOL_GATEWAY=1`, `gateway.enabled` in the Helm chart, or the switch on the server's page. Members switch over at their next sync, within a minute.

![The gateway on the server's page]({{ '/assets/server.png' | relative_url }})

## What a member sees

One account per provider the pool can serve them, named `team gateway (openai)`, `team gateway (claude)`, and so on. Starting an agent on it sets two variables: the provider's base address, pointing at the server, and the provider's key, holding the member's own session.

```bash
llmpool exec -- codex                                  # OPENAI_BASE_URL and OPENAI_API_KEY point at the server
llmpool run --account "team gateway (claude)"          # Claude Code through the gateway
```

Anything that speaks a provider's API can use it directly:

```bash
curl https://pool.example.com/gateway/openai/v1/chat/completions \
  -H "Authorization: Bearer $MY_LLMPOOL_KEY" -d '{"model": "...", "messages": [...]}'
```

## Which account a request runs on

The server keeps a member on the account they were last on while it has room, which keeps the provider's prompt cache warm. It moves to another account when that one passes 90% of a window, when the provider refuses it for being over its rate (it is then left alone for as long as the provider says), or when it fails. If one account refuses a request, the same request is tried on the next, up to three. The agent sees none of this, and nothing restarts.

For Claude subscriptions the server reads how full each account is from the provider's own answers.

## Traces

Each request leaves one line in `gateway-traces.log` in the server's data directory, and on standard error:

```json
{"at": 1791896274, "who": "ann@example.com", "provider": "claude", "account": "team-1@example.com",
 "model": "claude-opus-5-5", "path": "v1/messages", "status": 200, "ms": 3049,
 "input_tokens": 1820, "output_tokens": 412, "streamed": true, "tried": []}
```

Who, which account, which model, how long, how many tokens. Never what was said: bodies are passed through and not kept.

## What changes for Claude Code

Through a gateway Claude Code runs as an API client: it says "API usage billing", and its claude.ai connectors are off. Streaming, tools and prompt caching work as before. If those features matter more than keeping logins on the server, leave the gateway off and let the server lend instead.

## Your own proxy, or another provider

`LLMPOOL_UPSTREAM_<PROVIDER>` (the chart's `gateway.upstreams`) moves a provider's address: `LLMPOOL_UPSTREAM_OPENAI=https://llm-proxy.internal`. It also teaches the gateway a provider it does not know.
