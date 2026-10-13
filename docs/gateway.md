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

## What it carries, and what it does not

The gateway carries requests to models: messages, chat completions, responses, embeddings, token counts, and listing models. Everything else an account can do at its provider (files, stored work, fine-tuning, the account's own keys and billing) is refused here and never reaches the provider. A member borrows the pool's capacity, not its accounts.

One member may have 16 requests in flight at once; beyond that they are told to wait, and nobody else is affected. If a provider refuses an account for one member's request, the account is set aside for that member only.

## Which account a request runs on

The server keeps a member on the account they were last on while it has room, which keeps the provider's prompt cache warm. It moves to another account when that one passes 90% of a window, when the provider refuses it for being over its rate (it is then left alone for as long as the provider says), or when it fails. If one account refuses a request, the same request is tried on the next, up to three. The agent sees none of this, and nothing restarts.

For Claude subscriptions the server reads how full each account is from the provider's own answers.

### With the decision model

Turn the [decision model]({{ '/decisions/' | relative_url }}) on (the server's page, **Server → Decision model**) and the gateway stops waiting for 90%. When a member's account passes half of a window and there is another to choose, the server shows the model the whole pool, with how many members are on each account, and asks where that member's requests should go next. The answer holds for ten minutes and is recorded, so heavy use is spread across accounts before any one of them is close to its limit.

The model is asked after a request has been answered, never on a request's own time, at most once every five minutes per member and provider, and within the daily budget. If it is off, slow or silent, the rules above place every request.

## Requests and traces

The server's page has a **Requests** page: totals, tokens in and out, typical and slowest times, failures, requests a minute over the last hour, who sent what and which accounts carried it, every decision the model made, and the requests themselves, narrowed by member, provider, account or result.

![Requests on the server's page]({{ '/assets/requests.png' | relative_url }})

Each request is also one line in `gateway-traces.log` in the server's data directory, and on standard error for your log collector:

```json
{"at": 1791896274, "who": "ann@example.com", "provider": "claude", "account": "team-1@example.com",
 "model": "claude-opus-5-5", "path": "v1/messages", "status": 200, "ms": 3049,
 "input_tokens": 1820, "output_tokens": 412, "streamed": true, "tried": []}
```

Who, which account, which model, how long, how many tokens. Never what was said: bodies are passed through and not kept. The page keeps the newest 2,000; the file keeps all of them.

### To your own tracing

Set `OTEL_EXPORTER_OTLP_ENDPOINT` (the chart's `gateway.otlp.endpoint`) and every request is also sent to your OpenTelemetry collector as a span, over OTLP/HTTP, a batch every couple of seconds. `OTEL_EXPORTER_OTLP_HEADERS` (`name=value,name=value`; the chart's `gateway.otlp.headers`) carries whatever the collector asks for, values percent-encoded as that variable always is (`authorization=Bearer%20...`). A span is named `chat <model>` and carries `gen_ai.system`, `gen_ai.request.model`, `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `http.response.status_code`, `llmpool.member`, `llmpool.account` and `llmpool.attempts`. A collector that is slow or away loses spans and slows no request; the file on the server still has every line.

## How much it carries

Every request in flight has a thread of its own, so a long streamed answer never holds up another. On a laptop, 200 requests at once ran in about 50 MB of memory. `LLMPOOL_GATEWAY_MAX` (the chart's `gateway.maxInFlight`, 256 by default) caps how many may be in flight; beyond it the gateway answers 503 at once instead of queueing.

One server holds the pool, on purpose. A subscription login has a refresh token that works once: two processes renewing the same login would each invalidate the other's, and the login would be lost. So logins, renewal, sign-in and the server's page stay in one process.

### More than one process

What can be many is the part that carries requests. A **gateway replica** is the same program started with `LLMPOOL_REPLICA_OF=<the server's address>` (the chart's `gateway.replicas`). It keeps nothing: no login, no key, no file. For each member it asks the server who the token is and which accounts their request may run on, forwards the request itself, and tells the server how it went, so usage, switching, the decision model and the Requests page work exactly as with one process. Everything that is not a request to a model it passes on to the server.

- **No database, no second load balancer.** Replicas sit behind the same address. Each is a small process (see `gateway.replicaResources`).
- **Refresh tokens never reach a replica.** It is handed short-lived access tokens and keys, for the request in hand, over a channel closed by `LLMPOOL_REPLICA_KEY` (the chart makes one).
- **The server can restart under working agents.** A replica uses what the server last told it for fifteen seconds, and for up to five minutes while the server cannot be reached. In that window a member who was just turned away can still get through; sign-in and the server's page wait for the server.

`gateway.replicas: 0`, the default, is one process doing everything. That is enough for most teams: turn replicas on to carry more at once, or so a restart is not felt.

## Subscriptions first, a key when they are full

Add an Anthropic API key to the pool beside the subscription logins (`llmpool add key anthropic --to-team`, or the server's page) and Claude sessions overflow to it: requests run on subscriptions while one has room, and on the key only when every subscription is past 90% or cooling. A pool with nothing but a key carries Claude sessions on the key. The key is sent where Anthropic keys go (`LLMPOOL_UPSTREAM_ANTHROPIC` if you moved it), never to the subscriptions' address; a key stored with an address of its own is left out, since it may be another vendor's.

## What changes for Claude Code

Through a gateway Claude Code runs as an API client: it says "API usage billing", and its claude.ai connectors are off. Streaming, tools and prompt caching work as before. If those features matter more than keeping logins on the server, leave the gateway off and let the server lend instead.

## Your own proxy, or another provider

`LLMPOOL_UPSTREAM_<PROVIDER>` (the chart's `gateway.upstreams`) moves a provider's address: `LLMPOOL_UPSTREAM_OPENAI=https://llm-proxy.internal`. It also teaches the gateway a provider it does not know.
