//! Keeping a stored login alive. A login that sits unused eventually runs
//! out; renewing it now and then means it still works on the day it is
//! wanted. This is the only request llmpool ever makes to the provider, and
//! it is made only for logins no running Claude Code holds (one that is in
//! use renews itself, and two renewers would invalidate each other).

use crate::profile::Secret;
use serde_json::{json, Value};
use std::time::Duration;

pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Claude Code's public OAuth client id.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// What a subscription login is granted when nothing was recorded.
const DEFAULT_SCOPE: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// Renew when the login itself has less than this left.
const LOGIN_MARGIN_MS: i64 = 7 * 24 * 3600 * 1000;
/// And, so that a dead login is found before it is needed, when its short
/// access token ran out more than this long ago.
const STALE_MS: i64 = 24 * 3600 * 1000;

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// `secret` now holds the renewed login.
    Renewed,
    /// The provider says this login is over; a person has to sign in again.
    Dead,
    /// It could not be tried or the answer was unclear; try again later.
    Later(String),
}

/// Whether a stored login should be renewed now.
pub fn due(secret: &Secret, now_ms: i64) -> bool {
    let at = |key: &str| secret.oauth[key].as_i64();
    if secret.oauth["refreshToken"]
        .as_str()
        .is_none_or(str::is_empty)
    {
        return false;
    }
    at("refreshTokenExpiresAt").is_some_and(|t| t - now_ms < LOGIN_MARGIN_MS)
        || at("expiresAt").is_some_and(|t| now_ms - t > STALE_MS)
}

/// Renew `secret` in place. On anything but `Renewed` it is left untouched.
pub fn renew(secret: &mut Secret, url: &str, now_ms: i64) -> Outcome {
    // Nothing to renew with is not worth a request: the provider only answers
    // "invalid request format", which says nothing about the login.
    let Some(refresh_token) = secret.oauth["refreshToken"]
        .as_str()
        .filter(|token| !token.is_empty())
    else {
        return Outcome::Later("this login has no refresh token".into());
    };
    // The request Claude Code itself makes: the scopes the login was granted
    // go along, and the provider refuses the request without them.
    let scope = match secret.oauth["scopes"].as_array() {
        Some(scopes) if !scopes.is_empty() => scopes
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => DEFAULT_SCOPE.to_string(),
    };
    let request = json!({"grant_type": "refresh_token", "refresh_token": refresh_token, "client_id": CLIENT_ID, "scope": scope});
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();
    let response = agent
        .post(url)
        .set("User-Agent", concat!("llmpool/", env!("CARGO_PKG_VERSION")))
        .send_json(request);
    let body: Value = match response {
        Ok(response) => match response.into_json() {
            Ok(body) => body,
            Err(e) => return Outcome::Later(e.to_string()),
        },
        Err(ureq::Error::Status(code, response)) => {
            // Over only when the server says so in so many words: the
            // `error` member of the reply (RFC 6749 §5.2). Anything less
            // clear is tried again, because wrongly declaring a working
            // login dead costs far more than one more attempt.
            let body: Value = response.into_json().unwrap_or_default();
            // Either the plain OAuth form (`"error": "invalid_grant"`) or the
            // provider's own envelope (`"error": {"type": "invalid_grant"}`).
            let said = body["error"]
                .as_str()
                .or(body["error"]["type"].as_str())
                .unwrap_or_default();
            return if matches!(code, 400 | 401 | 403) && said == "invalid_grant" {
                Outcome::Dead
            } else {
                Outcome::Later(format!("the provider answered {code}"))
            };
        }
        Err(e) => return Outcome::Later(e.to_string()),
    };
    let (Some(access), Some(expires_in)) =
        (body["access_token"].as_str(), body["expires_in"].as_i64())
    else {
        return Outcome::Later("the provider's answer had no token in it".into());
    };
    let oauth = &mut secret.oauth;
    oauth["accessToken"] = json!(access);
    oauth["expiresAt"] = json!(now_ms + expires_in * 1000);
    // The refresh token is single-use: the reply carries its successor.
    if let Some(next) = body["refresh_token"].as_str() {
        oauth["refreshToken"] = json!(next);
    }
    if let Some(seconds) = body["refresh_token_expires_in"].as_i64() {
        oauth["refreshTokenExpiresAt"] = json!(now_ms + seconds * 1000);
    }
    if let Some(scope) = body["scope"].as_str() {
        oauth["scopes"] = json!(scope.split_whitespace().collect::<Vec<_>>());
    }
    Outcome::Renewed
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 3600 * 1000;

    fn secret(access_expires: i64, login_expires: i64) -> Secret {
        Secret {
            oauth: json!({"accessToken": "old", "refreshToken": "r-old", "expiresAt": access_expires, "refreshTokenExpiresAt": login_expires}),
            account: json!({"emailAddress": "a@x.io"}),
        }
    }

    /// A stand-in token endpoint that answers once.
    fn endpoint(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/token",
            server.server_addr().to_ip().unwrap().port()
        );
        let handle = std::thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let mut sent = String::new();
            request.as_reader().read_to_string(&mut sent).unwrap();
            request
                .respond(tiny_http::Response::from_string(body).with_status_code(status))
                .unwrap();
            sent
        });
        (url, handle)
    }

    #[test]
    fn renews_when_the_login_is_close_to_running_out_or_long_unused() {
        let now = 100 * DAY;
        assert!(
            !due(&secret(now + 3_600_000, now + 20 * DAY), now),
            "fresh, plenty of time"
        );
        assert!(
            due(&secret(now + 3_600_000, now + 6 * DAY), now),
            "login runs out within a week"
        );
        assert!(
            due(&secret(now - 2 * DAY, now + 20 * DAY), now),
            "unused for two days"
        );
        assert!(
            !due(&secret(now - 3_600_000, now + 20 * DAY), now),
            "expired an hour ago is normal"
        );
        let mut none = secret(now - 2 * DAY, now + DAY);
        none.oauth["refreshToken"] = Value::Null;
        assert!(!due(&none, now), "nothing to renew with");
    }

    #[test]
    fn a_renewal_takes_the_new_tokens_and_times() {
        let (url, sent) = endpoint(
            200,
            r#"{"access_token": "new", "refresh_token": "r-new", "expires_in": 3600, "refresh_token_expires_in": 86400, "scope": "a b"}"#,
        );
        let mut s = secret(0, 0);
        assert_eq!(renew(&mut s, &url, 1_000_000), Outcome::Renewed);
        assert_eq!(s.oauth["accessToken"], "new");
        assert_eq!(s.oauth["refreshToken"], "r-new");
        assert_eq!(s.oauth["expiresAt"], 1_000_000 + 3_600_000);
        assert_eq!(s.oauth["refreshTokenExpiresAt"], 1_000_000 + 86_400_000);
        assert_eq!(s.oauth["scopes"], json!(["a", "b"]));
        let sent: Value = serde_json::from_str(&sent.join().unwrap()).unwrap();
        assert_eq!(sent["grant_type"], "refresh_token");
        // Without the scopes the real provider answers "Invalid request format".
        assert_eq!(sent["scope"], DEFAULT_SCOPE);
        assert_eq!(sent["refresh_token"], "r-old");
    }

    #[test]
    fn only_an_explicit_refusal_means_the_login_is_over() {
        let before = secret(0, 0);
        for (status, body, expected_dead) in [
            (400, r#"{"error": "invalid_grant"}"#, true),
            (
                400,
                r#"{"type": "error", "error": {"type": "invalid_grant", "message": "Refresh token not found or invalid"}}"#,
                true,
            ),
            (
                400,
                r#"{"type": "error", "error": {"type": "invalid_request_error", "message": "Invalid request format"}}"#,
                false,
            ),
            (
                400,
                r#"{"error": "invalid_request", "error_description": "not invalid_grant"}"#,
                false,
            ),
            (500, r#"{"error": "invalid_grant"}"#, false),
            (429, "slow down", false),
            (200, r#"{"unexpected": true}"#, false),
        ] {
            let (url, sent) = endpoint(status, body);
            let mut s = before.clone();
            let outcome = renew(&mut s, &url, 1);
            sent.join().unwrap();
            assert_eq!(outcome == Outcome::Dead, expected_dead, "{status} {body}");
            assert_eq!(
                s, before,
                "an unrenewed login must be left exactly as it was"
            );
        }
    }

    #[test]
    fn the_scopes_the_login_was_granted_are_sent_back() {
        let (url, sent) = endpoint(200, r#"{"access_token": "new", "expires_in": 3600}"#);
        let mut s = secret(0, 0);
        s.oauth["scopes"] = json!(["user:inference", "user:profile"]);
        assert_eq!(renew(&mut s, &url, 1), Outcome::Renewed);
        let sent: Value = serde_json::from_str(&sent.join().unwrap()).unwrap();
        assert_eq!(sent["scope"], "user:inference user:profile");
        // No new refresh token in the reply: the old one stays.
        assert_eq!(s.oauth["refreshToken"], "r-old");
    }

    #[test]
    fn a_login_with_nothing_to_renew_with_is_not_sent() {
        // An address nothing listens on: reaching it would be a different message.
        let mut s = secret(0, 0);
        s.oauth["refreshToken"] = json!("");
        assert_eq!(
            renew(&mut s, "http://127.0.0.1:1/token", 1),
            Outcome::Later("this login has no refresh token".into())
        );
    }

    #[test]
    fn an_unreachable_provider_is_tried_again_later() {
        let mut s = secret(0, 0);
        assert!(matches!(
            renew(&mut s, "http://127.0.0.1:1/token", 1),
            Outcome::Later(_)
        ));
    }
}
