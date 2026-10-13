use llmpool_core::model::{Usage, Window};
use serde_json::Value;

/// Usage windows from the JSON Claude Code feeds its status line.
/// `rate_limits` is only present on subscription plans, after the first
/// response of a session; `None` means "no reading", not "unused".
pub fn usage(input: &Value, now: i64) -> Option<Usage> {
    let limits = input.get("rate_limits")?;
    let mut usage = Usage {
        fetched_at: now,
        ..Usage::default()
    };
    for (field, name) in [("five_hour", "5h"), ("seven_day", "7d")] {
        if let Some(percent) = limits
            .get(field)
            .and_then(|w| w.get("used_percentage"))
            .and_then(Value::as_f64)
        {
            // Whole seconds, but tolerate a fractional or textual timestamp: a
            // window with no reset time would otherwise never expire.
            let at = &limits[field]["resets_at"];
            let resets_at = at
                .as_i64()
                .or_else(|| at.as_f64().map(|f| f as i64))
                .or_else(|| at.as_str()?.parse().ok());
            usage.windows.insert(
                name.to_string(),
                Window {
                    used_percent: percent,
                    resets_at,
                },
            );
        }
    }
    (!usage.windows.is_empty()).then_some(usage)
}

pub fn model(input: &Value) -> Option<String> {
    let model = input.get("model")?;
    model
        .get("id")
        .or(Some(model))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_both_windows() {
        let input = json!({"model": {"id": "claude-opus-5-5"}, "rate_limits": {
            "five_hour": {"used_percentage": 91.5, "resets_at": 1700},
            "seven_day": {"used_percentage": 12, "resets_at": 9000}}});
        let u = usage(&input, 5).unwrap();
        assert_eq!(
            u.windows["5h"],
            Window {
                used_percent: 91.5,
                resets_at: Some(1700)
            }
        );
        assert_eq!(u.windows["7d"].used_percent, 12.0);
        assert_eq!(u.fetched_at, 5);
        assert_eq!(model(&input).as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn reset_time_is_read_whatever_number_form_it_takes() {
        for at in [json!(1700.9), json!("1700")] {
            let input =
                json!({"rate_limits": {"five_hour": {"used_percentage": 100, "resets_at": at}}});
            assert_eq!(
                usage(&input, 0).unwrap().windows["5h"].resets_at,
                Some(1700)
            );
        }
    }

    #[test]
    fn no_rate_limits_is_no_reading() {
        assert_eq!(usage(&json!({"model": "x"}), 0), None);
        assert_eq!(usage(&json!({"rate_limits": {}}), 0), None);
        assert_eq!(model(&json!({"model": "x"})).as_deref(), Some("x"));
    }
}
