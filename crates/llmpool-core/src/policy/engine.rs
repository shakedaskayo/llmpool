use super::context::{Context, Event};
use super::rule::{PolicyFile, Rule, Settings};
use super::{select, triggers};
use crate::model::{Account, AccountId, Session};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// No rule fired.
    NoMatch,
    /// A rule fired and said to do nothing.
    Hold,
    /// A rule fired with only `notify`.
    Notify,
    Assign(AccountId),
    Swap(AccountId),
    /// The user should choose among these.
    Ask(Vec<AccountId>),
    /// A rule fired but no account qualifies.
    Exhausted,
    /// A swap rule fired inside the session's cooldown.
    Cooldown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Decision {
    /// Name of the rule that fired.
    pub rule: Option<String>,
    pub outcome: Outcome,
    pub notify: bool,
}

/// Walk the rules in order; the first enabled rule whose trigger matches and
/// whose conditions hold decides.
pub fn evaluate(policy: &PolicyFile, event: Event, ctx: &Context) -> Decision {
    for rule in policy.rules.iter().filter(|r| r.enabled) {
        let Some(trigger) = triggers::find(&rule.on.kind) else {
            continue;
        };
        if !trigger.matches(&rule.on.params, event, ctx) || !rule.when.hold(ctx) {
            continue;
        }
        let threshold = trigger.threshold(&rule.on.params);
        return Decision {
            rule: Some(rule.name.clone()),
            outcome: run(rule, threshold, &policy.settings, ctx),
            notify: rule.action.notify,
        };
    }
    Decision { rule: None, outcome: Outcome::NoMatch, notify: false }
}

fn run(rule: &Rule, threshold: Option<(String, f64)>, settings: &Settings, ctx: &Context) -> Outcome {
    let action = &rule.action;
    let ceiling = threshold.as_ref().map(|(window, percent)| (window.as_str(), *percent));
    let chosen = |account: Option<&Account>, wrap: fn(AccountId) -> Outcome| {
        account.map_or(Outcome::Exhausted, |a| wrap(a.id.clone()))
    };

    if action.hold {
        return Outcome::Hold;
    }
    if let Some(target) = &action.assign {
        let found = select::candidates(target, ctx, ceiling);
        return chosen(select::pick(target.pick, found, ctx.rr_cursor), Outcome::Assign);
    }
    if let Some(target) = &action.ask {
        let ids: Vec<_> = select::candidates(target, ctx, ceiling).iter().map(|a| a.id.clone()).collect();
        return if ids.is_empty() { Outcome::Exhausted } else { Outcome::Ask(ids) };
    }
    if let Some(target) = &action.swap {
        if ctx.session.last_swap_at.is_some_and(|at| ctx.now - at < settings.cooldown_secs) {
            return Outcome::Cooldown;
        }
        let mut found = select::candidates(target, ctx, ceiling);
        // A usage-driven swap must be worth it: the target has to beat the
        // current account by the margin, or two accounts near the line ping-pong.
        if let (Some((window, _)), Some(current)) = (ceiling, ctx.current()) {
            found.retain(|a| a.used(window) + settings.margin_points <= current.used(window));
        }
        return chosen(select::pick(target.pick, found, ctx.rr_cursor), Outcome::Swap);
    }
    Outcome::Notify
}

fn monday() -> String {
    "mon".into()
}

fn noon() -> u8 {
    12
}

/// A self-contained situation to evaluate a policy against. Used by
/// `llmpool policy dry-run` and by the GUI's rule preview.
#[derive(Clone, Debug, Deserialize)]
pub struct Scenario {
    pub event: Event,
    pub session: Session,
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub now: i64,
    #[serde(default = "monday")]
    pub weekday: String,
    #[serde(default = "noon")]
    pub hour: u8,
    #[serde(default)]
    pub home: String,
}

impl Scenario {
    pub fn decide(&self, policy: &PolicyFile) -> Decision {
        evaluate(
            policy,
            self.event,
            &Context {
                session: &self.session,
                accounts: &self.accounts,
                now: self.now,
                weekday: &self.weekday,
                hour: self.hour,
                home: &self.home,
                rr_cursor: None,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{account, ctx, session};

    fn policy(text: &str) -> PolicyFile {
        PolicyFile::parse(text).unwrap()
    }

    const SWAP_AT_90: &str = r#"
[[rule]]
name = "swap before the limit"
on = { usage_above = { percent = 90 } }
do = { swap = {}, notify = true }
"#;

    #[test]
    fn no_rule_is_no_match() {
        let s = session("/w", None);
        let d = evaluate(&PolicyFile::default(), Event::SessionStart, &ctx(&s, &[]));
        assert_eq!(d, Decision { rule: None, outcome: Outcome::NoMatch, notify: false });
    }

    #[test]
    fn first_matching_rule_wins_and_disabled_rules_are_skipped() {
        let p = policy(
            r#"
[[rule]]
name = "off"
enabled = false
on = "session_start"
do = { hold = true }

[[rule]]
name = "acme"
on = "session_start"
when = { cwd = "/work/acme/**" }
do = { assign = { tag = "acme" } }

[[rule]]
name = "rest"
on = "session_start"
do = { ask = {} }
"#,
        );
        let accounts = [account("a1", &["acme"], None), account("p1", &[], None)];

        let s = session("/work/acme/api", None);
        let d = evaluate(&p, Event::SessionStart, &ctx(&s, &accounts));
        assert_eq!(d.rule.as_deref(), Some("acme"));
        assert_eq!(d.outcome, Outcome::Assign("a1".into()));

        let s = session("/elsewhere", None);
        let d = evaluate(&p, Event::SessionStart, &ctx(&s, &accounts));
        assert_eq!(d.rule.as_deref(), Some("rest"));
        assert_eq!(d.outcome, Outcome::Ask(vec!["a1".into(), "p1".into()]));
    }

    #[test]
    fn hold_stops_evaluation() {
        let p = policy(
            r#"
[[rule]]
name = "leave this repo alone"
on = "rate_limited"
when = { cwd = "/pinned/**" }
do = { hold = true }

[[rule]]
name = "swap"
on = "rate_limited"
do = { swap = {} }
"#,
        );
        let accounts = [account("a", &[], None), account("b", &[], None)];
        let s = session("/pinned/x", Some("a"));
        assert_eq!(evaluate(&p, Event::RateLimited, &ctx(&s, &accounts)).outcome, Outcome::Hold);
    }

    #[test]
    fn threshold_swap_needs_the_margin() {
        let p = policy(SWAP_AT_90);
        let s = session("/w", Some("cur"));

        // 85 is under the ceiling but not 10 points better than 91.
        let accounts = [account("cur", &[], Some(91.0)), account("near", &[], Some(85.0))];
        let d = evaluate(&p, Event::UsageUpdated, &ctx(&s, &accounts));
        assert_eq!(d.outcome, Outcome::Exhausted);
        assert!(d.notify);

        let accounts = [
            account("cur", &[], Some(91.0)),
            account("near", &[], Some(85.0)),
            account("roomy", &[], Some(50.0)),
        ];
        let d = evaluate(&p, Event::UsageUpdated, &ctx(&s, &accounts));
        assert_eq!(d.outcome, Outcome::Swap("roomy".into()));
    }

    #[test]
    fn rate_limited_swap_ignores_the_margin() {
        let p = policy("[[rule]]\nname = 'rl'\non = 'rate_limited'\ndo = { swap = {} }\n");
        let accounts = [account("cur", &[], Some(91.0)), account("near", &[], Some(85.0))];
        let s = session("/w", Some("cur"));
        let d = evaluate(&p, Event::RateLimited, &ctx(&s, &accounts));
        assert_eq!(d.outcome, Outcome::Swap("near".into()));
    }

    #[test]
    fn cooldown_blocks_a_second_swap() {
        let p = policy(SWAP_AT_90);
        let accounts = [account("cur", &[], Some(95.0)), account("b", &[], Some(10.0))];
        let mut s = session("/w", Some("cur"));
        // The fixture context has now = 1_000_000 and the default cooldown is 300s.
        s.last_swap_at = Some(1_000_000 - 299);
        assert_eq!(evaluate(&p, Event::UsageUpdated, &ctx(&s, &accounts)).outcome, Outcome::Cooldown);
        s.last_swap_at = Some(1_000_000 - 300);
        assert_eq!(
            evaluate(&p, Event::UsageUpdated, &ctx(&s, &accounts)).outcome,
            Outcome::Swap("b".into())
        );
    }

    #[test]
    fn tag_no_account_carries_is_exhausted() {
        let p = policy(
            "[[rule]]\nname = 'ghost'\non = 'session_start'\ndo = { assign = { tag = 'ghost' } }\n",
        );
        let accounts = [account("a", &["work"], None)];
        let s = session("/w", None);
        let d = evaluate(&p, Event::SessionStart, &ctx(&s, &accounts));
        assert_eq!(d.rule.as_deref(), Some("ghost"));
        assert_eq!(d.outcome, Outcome::Exhausted);
    }

    #[test]
    fn notify_only_rule() {
        let p = policy("[[rule]]\nname = 'tell'\non = 'auth_failed'\ndo = { notify = true }\n");
        let s = session("/w", None);
        let d = evaluate(&p, Event::AuthFailed, &ctx(&s, &[]));
        assert_eq!(d.outcome, Outcome::Notify);
        assert!(d.notify);
    }

    #[test]
    fn scenario_decides_from_json() {
        let json = serde_json::json!({
            "event": "usage_updated",
            "session": session("/w", Some("cur")),
            "accounts": [account("cur", &[], Some(95.0)), account("b", &[], Some(10.0))],
        });
        let scenario: Scenario = serde_json::from_value(json).unwrap();
        let d = scenario.decide(&policy(SWAP_AT_90));
        assert_eq!(serde_json::to_value(&d).unwrap()["outcome"]["swap"], "b");
    }
}
