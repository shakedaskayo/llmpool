//! Gateway traces, sent on to an OpenTelemetry collector: OTLP over HTTP, as
//! JSON, one span for every request the gateway carried.

use serde_json::{json, Value};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::time::Duration;

/// Where spans go, and the headers the collector asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Otlp {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
}

impl Otlp {
    /// From the variables every OpenTelemetry exporter reads:
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_EXPORTER_OTLP_HEADERS`
    /// (`name=value,name=value`).
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let endpoint = var("OTEL_EXPORTER_OTLP_ENDPOINT")?;
        let endpoint = endpoint.trim().trim_end_matches('/').to_string();
        if endpoint.is_empty() {
            return None;
        }
        let headers = var("OTEL_EXPORTER_OTLP_HEADERS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|pair| {
                let (name, value) = pair.split_once('=')?;
                Some((name.trim().to_string(), value.trim().to_string()))
            })
            .filter(|(name, _)| !name.is_empty())
            .collect();
        Some(Self { endpoint, headers })
    }
}

/// One line of the gateway's trace log as an OTLP span, named and labelled
/// the way the generative-AI conventions do.
pub(super) fn span(line: &Value) -> Value {
    let text = |key: &str| line[key].as_str().unwrap_or_default().to_string();
    let number = |key: &str| line[key].as_u64().unwrap_or(0);
    let ended = number("at") * 1_000_000_000;
    let began = ended.saturating_sub(number("ms") * 1_000_000);
    let status = number("status");
    let string = |key: &str, value: String| json!({"key": key, "value": {"stringValue": value}});
    let int = |key: &str, value: u64| json!({"key": key, "value": {"intValue": value.to_string()}});
    json!({
        "traceId": super::random_hex(16),
        "spanId": super::random_hex(8),
        "name": format!("chat {}", text("model")).trim().to_string(),
        "kind": 3,
        "startTimeUnixNano": began.to_string(),
        "endTimeUnixNano": ended.to_string(),
        "attributes": [
            string("gen_ai.system", text("provider")),
            string("gen_ai.request.model", text("model")),
            int("gen_ai.usage.input_tokens", number("input_tokens")),
            int("gen_ai.usage.output_tokens", number("output_tokens")),
            int("http.response.status_code", status),
            string("llmpool.member", text("who")),
            string("llmpool.account", text("account")),
            int("llmpool.attempts", line["tried"].as_array().map_or(0, |t| t.len() as u64) + 1),
        ],
        "status": {"code": if status >= 400 { 2 } else { 1 }},
    })
}

/// Send what arrives to the collector, a batch every couple of seconds.
/// shortcut: a batch the collector refuses is dropped, not retried; add a
/// retry queue if spans must survive a collector restart.
pub(super) fn start(to: Otlp) -> SyncSender<Value> {
    let (send, receive) = sync_channel::<Value>(4096);
    std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(10))
            .build();
        let mut spans = Vec::new();
        loop {
            let closed = match receive.recv_timeout(Duration::from_secs(2)) {
                Ok(line) => {
                    spans.push(span(&line));
                    if spans.len() < 200 {
                        continue;
                    }
                    false
                }
                Err(RecvTimeoutError::Timeout) => false,
                Err(RecvTimeoutError::Disconnected) => true,
            };
            if !spans.is_empty() {
                let mut call = agent.post(&format!("{}/v1/traces", to.endpoint));
                for (name, value) in &to.headers {
                    call = call.set(name, value);
                }
                let body = json!({"resourceSpans": [{
                    "resource": {"attributes": [
                        {"key": "service.name", "value": {"stringValue": "llmpool"}}]},
                    "scopeSpans": [{
                        "scope": {"name": "llmpool.gateway"},
                        "spans": std::mem::take(&mut spans)}],
                }]});
                if let Err(e) = call.send_json(body) {
                    eprintln!("traces were not taken by the collector: {e}");
                }
            }
            if closed {
                return;
            }
        }
    });
    send
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_variables_say_where_spans_go() {
        let var = |name: &str| match name {
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(" https://otel.example/ ".to_string()),
            "OTEL_EXPORTER_OTLP_HEADERS" => Some("authorization=Bearer x, x-team=a".to_string()),
            _ => None,
        };
        let to = Otlp::from_env(var).unwrap();
        assert_eq!(to.endpoint, "https://otel.example");
        assert_eq!(to.headers[0], ("authorization".into(), "Bearer x".into()));
        assert_eq!(to.headers.len(), 2);
        assert_eq!(Otlp::from_env(|_| None), None);
    }

    #[test]
    fn a_failed_request_is_a_span_in_error_that_ends_when_it_did() {
        let s = span(&json!({"at": 100, "ms": 1500, "status": 502, "model": "m", "tried": ["a"]}));
        assert_eq!(s["status"]["code"], 2);
        assert_eq!(s["endTimeUnixNano"], "100000000000");
        assert_eq!(s["startTimeUnixNano"], "98500000000");
    }
}
