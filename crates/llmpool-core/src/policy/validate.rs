use super::rule::PolicyFile;
use super::triggers;

/// Every problem in a parsed policy, one message each. Empty means valid.
pub fn validate(policy: &PolicyFile) -> Vec<String> {
    let mut errors = Vec::new();

    if policy.settings.cooldown_secs < 0 {
        errors.push("settings: `cooldown_secs` must not be negative".to_string());
    }
    if !(0.0..=100.0).contains(&policy.settings.margin_points) {
        errors.push("settings: `margin_points` must be 0 to 100".to_string());
    }
    if !(1.0..=100.0).contains(&policy.settings.own_until) {
        errors.push("settings: `own_until` must be 1 to 100".to_string());
    }
    let advisor = &policy.advisor;
    if advisor.provider != "jev" {
        errors.push(format!(
            "advisor: unknown provider `{}`; only \"jev\" is available",
            advisor.provider
        ));
    }
    if !(0.5..=1.0).contains(&advisor.switch_above) {
        errors.push("advisor: `switch_above` must be 0.5 to 1".to_string());
    }
    if advisor.min_interval_secs < 30 {
        errors.push("advisor: `min_interval_secs` must be at least 30".to_string());
    }

    for (index, rule) in policy.rules.iter().enumerate() {
        let mut fail = |message: String| {
            errors.push(format!("rule {} ({:?}): {message}", index + 1, rule.name))
        };

        match triggers::find(&rule.on.kind) {
            None => fail(format!("unknown trigger `{}`", rule.on.kind)),
            Some(trigger) => {
                if let Err(e) = trigger.check(&rule.on.params) {
                    fail(e);
                }
            }
        }
        if let Err(e) = rule.when.check() {
            fail(e);
        }

        let action = &rule.action;
        let targets = [&action.assign, &action.swap, &action.ask];
        let chosen = targets.iter().filter(|t| t.is_some()).count() + usize::from(action.hold);
        if chosen > 1 {
            fail("`do` takes only one of assign, swap, ask, hold".into());
        } else if chosen == 0 && !action.notify {
            fail("`do` is empty".into());
        }
        if action.assign.is_some() && rule.on.kind != "session_start" {
            fail("`assign` is only valid on `session_start`; use `swap`".into());
        }
        if targets.iter().any(|t| {
            t.as_ref()
                .is_some_and(|t| t.account.is_some() && t.tag.is_some())
        }) {
            fail("give `account` or `tag`, not both".into());
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(text: &str) -> Vec<String> {
        validate(&PolicyFile::parse(text).unwrap())
    }

    fn rule(body: &str) -> String {
        format!("[[rule]]\nname = 'r'\n{body}\n")
    }

    #[test]
    fn a_good_policy_has_no_errors() {
        let text = rule("on = { usage_above = { percent = 90 } }\ndo = { swap = { tag = 'same' }, notify = true }");
        assert_eq!(errors(&text), Vec::<String>::new());
    }

    #[test]
    fn reports_each_problem_with_the_rule_position_and_name() {
        let text = [
            rule("on = 'session_start'\ndo = { hold = true }"),
            rule("on = 'moon_phase'\ndo = { hold = true }"),
            rule("on = { usage_above = { percent = 150 } }\ndo = { notify = true }"),
        ]
        .concat();
        let found = errors(&text);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("rule 2 (\"r\")"), "{}", found[0]);
        assert!(found[0].contains("moon_phase"));
        assert!(found[1].starts_with("rule 3 (\"r\")"), "{}", found[1]);
    }

    #[test]
    fn action_must_be_exactly_one_thing() {
        assert_eq!(errors(&rule("on = 'rate_limited'\ndo = {}")).len(), 1);
        assert_eq!(
            errors(&rule(
                "on = 'rate_limited'\ndo = { swap = {}, hold = true }"
            ))
            .len(),
            1
        );
        assert_eq!(
            errors(&rule("on = 'rate_limited'\ndo = { swap = {}, ask = {} }")).len(),
            1
        );
    }

    #[test]
    fn assign_only_on_session_start() {
        assert_eq!(
            errors(&rule("on = 'rate_limited'\ndo = { assign = {} }")).len(),
            1
        );
        assert!(errors(&rule("on = 'session_start'\ndo = { assign = {} }")).is_empty());
    }

    #[test]
    fn target_takes_account_or_tag_not_both() {
        let text = rule("on = 'rate_limited'\ndo = { swap = { account = 'a', tag = 'b' } }");
        assert_eq!(errors(&text).len(), 1);
    }

    #[test]
    fn bad_condition_and_settings_are_reported() {
        assert_eq!(
            errors(&rule(
                "on = 'rate_limited'\nwhen = { cwd = '[' }\ndo = { hold = true }"
            ))
            .len(),
            1
        );
        assert_eq!(
            errors("[settings]\ncooldown_secs = -1\nmargin_points = 500\n").len(),
            2
        );
    }
}
