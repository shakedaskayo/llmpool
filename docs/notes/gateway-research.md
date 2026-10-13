# An LLM gateway beside the server: what was looked at

Asked for: let people who install the server optionally use it as a gateway
that routes requests between accounts and collects traces, natively if a Rust
implementation of LiteLLM allows it. Looked at on 2026-10-13; nothing here was
run, and nothing is built yet.

## What exists

| | Form | State |
|---|---|---|
| LiteLLM's own Rust gateway (`litellm-rust/` in BerriAI/litellm, MIT) | about 70 crates, an axum/tokio binary `litellm-gateway` with a Dockerfile | Mid-migration, with a full pure-Rust server planned for December 2026. Not on crates.io. Virtual keys, budgets and spend are not there yet; no OpenTelemetry exporter was found. It does forward Claude subscription tokens upstream as bearer tokens. |
| agentgateway (Apache-2.0) | binary and container, YAML or a Kubernetes controller | Released regularly. OpenAI-compatible and Anthropic endpoints, load balancing, failover, budgets, OpenTelemetry metrics, logs and traces. The upstream key can be an expression computed per request. |
| litellm-rs (MIT, one maintainer, unaffiliated) | crate, binary and image | Usable, with the risk of a single maintainer. |
| TensorZero | | Archived. |
| Helicone ai-gateway | | GPL-3.0 and stale. |

## What follows

- **Not embedded.** Every candidate is built on tokio and axum; `llmpool serve`
  is a small synchronous server. Embedding means a second runtime and hundreds
  of dependencies pulled from unpublished git crates.
- **A sidecar in the Helm chart, off by default.** The gateway runs next to the
  server; llmpool writes its configuration from the pool's keys and tells it
  per request which account to use.
- **Which one.** agentgateway today, because it is released and has the traces
  and the per-request key. LiteLLM's Rust gateway is the natural choice once
  it ships keys, budgets and a release; revisit after its December milestone.

## To decide before building

- A gateway sits between the agent and the provider. For keys that is
  ordinary. For Claude subscription logins it is what the core design chose
  not to do (Claude Code features that depend on the first-party host, and
  the provider's terms), so the proposal is: keys through the gateway, logins
  lent as they are now.
- Access tokens would be shared with a second process. Today nothing but
  llmpool holds them.
- The per-request key expression in agentgateway was read in its source, not
  exercised.
