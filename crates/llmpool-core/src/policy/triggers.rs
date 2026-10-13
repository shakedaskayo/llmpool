use super::context::{Context, Event};
use serde::Serialize;

/// Describes a trigger's parameters so a GUI can render an editor for them.
pub type ParamSchema = Vec<Field>;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Field {
    pub name: &'static str,
    pub kind: FieldKind,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Percent,
}

/// To add a trigger: implement this and add one line to `all()`.
pub trait Trigger: Send + Sync {
    /// Name used in a rule's `on` field.
    fn kind(&self) -> &'static str;

    fn matches(&self, params: &toml::Table, event: Event, ctx: &Context) -> bool;

    fn schema(&self) -> ParamSchema {
        Vec::new()
    }

    /// Validate a rule's parameters for this trigger.
    fn check(&self, params: &toml::Table) -> Result<(), String> {
        match params.keys().next() {
            Some(key) => Err(format!("`{}` takes no parameter `{key}`", self.kind())),
            None => Ok(()),
        }
    }

    /// The (window, percent) this trigger fires at, if it is usage driven.
    /// The engine uses it to filter and rank swap candidates.
    fn threshold(&self, _params: &toml::Table) -> Option<(String, f64)> {
        None
    }
}

pub fn all() -> Vec<Box<dyn Trigger>> {
    vec![
        Box::new(OnEvent("session_start", Event::SessionStart)),
        Box::new(OnEvent("rate_limited", Event::RateLimited)),
        Box::new(OnEvent("auth_failed", Event::AuthFailed)),
        Box::new(OnEvent("cwd_changed", Event::CwdChanged)),
        Box::new(UsageAbove),
        Box::new(Schedule),
        Box::new(Pressure),
    ]
}

pub fn find(kind: &str) -> Option<Box<dyn Trigger>> {
    all().into_iter().find(|t| t.kind() == kind)
}

/// Fires whenever its event occurs.
struct OnEvent(&'static str, Event);

impl Trigger for OnEvent {
    fn kind(&self) -> &'static str {
        self.0
    }

    fn matches(&self, _params: &toml::Table, event: Event, _ctx: &Context) -> bool {
        event == self.1
    }
}

/// Fires at one time of day: `{ at = "09:00", days = ["mon", "tue"] }`.
/// `days` is optional; without it, every day.
struct Schedule;

impl Schedule {
    fn parse(params: &toml::Table) -> Result<(u8, u8, Vec<String>), String> {
        if let Some(key) = params.keys().find(|k| !matches!(k.as_str(), "at" | "days")) {
            return Err(format!("`schedule` takes no parameter `{key}`"));
        }
        let at = params
            .get("at")
            .and_then(|v| v.as_str())
            .ok_or("`schedule` needs `at`, a time such as \"09:00\"")?;
        let time = at
            .split_once(':')
            .and_then(|(h, m)| Some((h.parse::<u8>().ok()?, m.parse::<u8>().ok()?)));
        let Some((hour, minute)) = time.filter(|(h, m)| *h < 24 && *m < 60) else {
            return Err(format!(
                "`schedule.at` must be a time such as \"09:00\", got \"{at}\""
            ));
        };
        let days = match params.get("days") {
            None => Vec::new(),
            Some(toml::Value::Array(days)) => days
                .iter()
                .filter_map(|d| d.as_str().map(str::to_string))
                .collect(),
            Some(_) => {
                return Err("`schedule.days` must be a list such as [\"mon\", \"tue\"]".into())
            }
        };
        if let Some(day) = days
            .iter()
            .find(|d| !super::conditions::DAYS.contains(&d.as_str()))
        {
            return Err(format!(
                "`schedule.days`: unknown day `{day}`, use mon..sun"
            ));
        }
        Ok((hour, minute, days))
    }
}

impl Trigger for Schedule {
    fn kind(&self) -> &'static str {
        "schedule"
    }

    fn matches(&self, params: &toml::Table, event: Event, ctx: &Context) -> bool {
        let (Event::Tick, Ok((hour, minute, days))) = (event, Self::parse(params)) else {
            return false;
        };
        hour == ctx.hour
            && minute == ctx.minute
            && (days.is_empty() || days.iter().any(|d| d == ctx.weekday))
    }

    fn schema(&self) -> ParamSchema {
        vec![Field {
            name: "at",
            kind: FieldKind::Text,
        }]
    }

    fn check(&self, params: &toml::Table) -> Result<(), String> {
        Self::parse(params).map(|_| ())
    }
}

/// Fires when an account is being spent so fast that its window will be full
/// before it resets: `{ window = "5h", full_within_minutes = 30 }`. With
/// `min_sessions = 2` only when that many sessions share the account, which is
/// the case of several people or agents leaning on one subscription.
struct Pressure;

impl Pressure {
    fn parse(params: &toml::Table) -> Result<(String, i64, f64, usize), String> {
        if let Some(key) = params.keys().find(|k| {
            !matches!(
                k.as_str(),
                "window" | "full_within_minutes" | "min_sessions"
            )
        }) {
            return Err(format!("`pressure` takes no parameter `{key}`"));
        }
        let (window, secs) = match params.get("window").map(|v| v.as_str()) {
            None | Some(Some("5h")) => ("5h", 5 * 3600),
            Some(Some("7d")) => ("7d", 7 * 24 * 3600),
            _ => return Err("`pressure.window` must be \"5h\" or \"7d\"".into()),
        };
        let number = |key: &str, default: f64| match params.get(key) {
            None => Ok(default),
            Some(toml::Value::Integer(i)) => Ok(*i as f64),
            Some(toml::Value::Float(f)) => Ok(*f),
            Some(_) => Err(format!("`pressure.{key}` must be a number")),
        };
        let minutes = number("full_within_minutes", 30.0)?;
        let sessions = number("min_sessions", 1.0)?;
        if !(1.0..=10_000.0).contains(&minutes) || !(1.0..=1000.0).contains(&sessions) {
            return Err(
                "`pressure`: `full_within_minutes` and `min_sessions` must be at least 1".into(),
            );
        }
        Ok((window.to_string(), secs, minutes, sessions as usize))
    }
}

impl Trigger for Pressure {
    fn kind(&self) -> &'static str {
        "pressure"
    }

    fn matches(&self, params: &toml::Table, event: Event, ctx: &Context) -> bool {
        let (Event::UsageUpdated, Ok((window, window_secs, minutes, sessions)), Some(current)) =
            (event, Self::parse(params), ctx.current())
        else {
            return false;
        };
        let (used, Some(reset)) = (current.used(&window), current.reset(&window)) else {
            return false;
        };
        let left = (reset - ctx.now) as f64;
        let spent = window_secs as f64 - left;
        // Too early in the window, or too little used, for the pace to mean anything.
        if spent < 600.0 || used < 5.0 || left <= 0.0 || ctx.load(&current.id) < sessions {
            return false;
        }
        let until_full = (100.0 - used) / (used / spent);
        until_full < minutes * 60.0 && until_full < left
    }

    fn schema(&self) -> ParamSchema {
        vec![
            Field {
                name: "window",
                kind: FieldKind::Text,
            },
            Field {
                name: "full_within_minutes",
                kind: FieldKind::Text,
            },
            Field {
                name: "min_sessions",
                kind: FieldKind::Text,
            },
        ]
    }

    fn check(&self, params: &toml::Table) -> Result<(), String> {
        Self::parse(params).map(|_| ())
    }
}

struct UsageAbove;

impl UsageAbove {
    fn parse(params: &toml::Table) -> Result<(String, f64), String> {
        if let Some(key) = params
            .keys()
            .find(|k| !matches!(k.as_str(), "window" | "percent"))
        {
            return Err(format!("`usage_above` takes no parameter `{key}`"));
        }
        let window = match params.get("window") {
            None => "5h".to_string(),
            Some(toml::Value::String(s)) => s.clone(),
            Some(_) => return Err("`usage_above.window` must be text".into()),
        };
        let percent = match params.get("percent") {
            Some(toml::Value::Integer(i)) => *i as f64,
            Some(toml::Value::Float(f)) => *f,
            _ => return Err("`usage_above` needs a numeric `percent`".into()),
        };
        if !(1.0..=100.0).contains(&percent) {
            return Err(format!(
                "`usage_above.percent` must be 1 to 100, got {percent}"
            ));
        }
        Ok((window, percent))
    }
}

impl Trigger for UsageAbove {
    fn kind(&self) -> &'static str {
        "usage_above"
    }

    fn matches(&self, params: &toml::Table, event: Event, ctx: &Context) -> bool {
        let (Event::UsageUpdated, Ok((window, percent)), Some(current)) =
            (event, Self::parse(params), ctx.current())
        else {
            return false;
        };
        current.used(&window) >= percent
    }

    fn schema(&self) -> ParamSchema {
        vec![
            Field {
                name: "window",
                kind: FieldKind::Text,
            },
            Field {
                name: "percent",
                kind: FieldKind::Percent,
            },
        ]
    }

    fn check(&self, params: &toml::Table) -> Result<(), String> {
        Self::parse(params).map(|_| ())
    }

    fn threshold(&self, params: &toml::Table) -> Option<(String, f64)> {
        Self::parse(params).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{account, ctx, session};

    fn params(text: &str) -> toml::Table {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn event_triggers_match_only_their_event() {
        let s = session("/w", None);
        let c = ctx(&s, &[]);
        let t = find("rate_limited").unwrap();
        assert!(t.matches(&toml::Table::new(), Event::RateLimited, &c));
        assert!(!t.matches(&toml::Table::new(), Event::AuthFailed, &c));
        assert!(t.check(&params("extra = 1")).is_err());
    }

    #[test]
    fn usage_above_reads_the_current_account() {
        let accounts = [account("a", &[], Some(91.0))];
        let s = session("/w", Some("a"));
        let c = ctx(&s, &accounts);
        let t = find("usage_above").unwrap();
        assert!(t.matches(
            &params("window = '5h'\npercent = 90"),
            Event::UsageUpdated,
            &c
        ));
        assert!(t.matches(&params("percent = 90"), Event::UsageUpdated, &c));
        assert!(!t.matches(&params("percent = 95"), Event::UsageUpdated, &c));
        assert!(!t.matches(&params("percent = 90"), Event::RateLimited, &c));
        assert_eq!(
            t.threshold(&params("percent = 90")),
            Some(("5h".to_string(), 90.0))
        );
    }

    #[test]
    fn usage_above_without_a_current_account_never_matches() {
        let s = session("/w", None);
        let c = ctx(&s, &[]);
        let t = find("usage_above").unwrap();
        assert!(!t.matches(&params("percent = 1"), Event::UsageUpdated, &c));
    }

    #[test]
    fn usage_above_rejects_bad_percent() {
        let t = find("usage_above").unwrap();
        assert!(t.check(&params("percent = 90")).is_ok());
        assert!(t.check(&params("percent = 150")).is_err());
        assert!(t.check(&params("percent = 0")).is_err());
        assert!(t.check(&params("window = '5h'")).is_err());
    }

    #[test]
    fn schedule_fires_on_its_minute_and_day_only() {
        // The fixture context is Tuesday, 10:30.
        let s = session("/w", None);
        let c = ctx(&s, &[]);
        let t = find("schedule").unwrap();
        assert!(t.matches(&params("at = '10:30'"), Event::Tick, &c));
        assert!(t.matches(&params("at = '10:30'\ndays = ['tue']"), Event::Tick, &c));
        assert!(!t.matches(&params("at = '10:31'"), Event::Tick, &c));
        assert!(!t.matches(&params("at = '10:30'\ndays = ['sat']"), Event::Tick, &c));
        assert!(!t.matches(&params("at = '10:30'"), Event::UsageUpdated, &c));
        assert!(t.check(&params("at = '09:00'")).is_ok());
        for bad in [
            "at = '25:00'",
            "at = 'nine'",
            "days = ['mon']",
            "at = '09:00'\ndays = ['funday']",
            "at = '09:00'\nextra = 1",
        ] {
            assert!(t.check(&params(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn pressure_fires_on_pace_not_on_a_fixed_percent() {
        use crate::model::Window;
        use std::collections::BTreeMap;
        let t = find("pressure").unwrap();
        // The fixture clock reads 1_000_000. Half of a 5-hour window is gone.
        let reading = |used: f64| {
            let mut a = account("a", &[], Some(used));
            a.usage.as_mut().unwrap().windows.insert(
                "5h".into(),
                Window {
                    used_percent: used,
                    resets_at: Some(1_000_000 + 9000),
                },
            );
            [a]
        };
        let s = session("/w", Some("a"));
        let fires = |used: f64, text: &str, busy: &BTreeMap<String, usize>| {
            let accounts = reading(used);
            let mut c = ctx(&s, &accounts);
            c.busy = busy;
            t.matches(&params(text), Event::UsageUpdated, &c)
        };
        let one: BTreeMap<String, usize> = [("a".to_string(), 1)].into();
        let two: BTreeMap<String, usize> = [("a".to_string(), 2)].into();
        // 90% in half the window: full in about 17 minutes, well before the reset.
        assert!(fires(90.0, "full_within_minutes = 30", &one));
        assert!(!fires(90.0, "full_within_minutes = 10", &one));
        // 40% in half the window lasts to the reset: no pressure, whatever the percent rule would say.
        assert!(!fires(40.0, "full_within_minutes = 600", &one));
        // Only when several sessions lean on the same account.
        assert!(!fires(90.0, "min_sessions = 2", &one));
        assert!(fires(90.0, "min_sessions = 2", &two));
        // Too little used to judge a pace.
        assert!(!fires(3.0, "full_within_minutes = 600", &one));

        assert!(t
            .check(&params(
                "window = '7d'\nfull_within_minutes = 120\nmin_sessions = 2"
            ))
            .is_ok());
        for bad in [
            "window = '1h'",
            "full_within_minutes = 0",
            "min_sessions = 0",
            "extra = 1",
            "full_within_minutes = 'soon'",
        ] {
            assert!(t.check(&params(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_kind_is_unique_and_findable() {
        let kinds: Vec<_> = all().iter().map(|t| t.kind()).collect();
        let mut unique = kinds.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), kinds.len());
        for kind in [
            "session_start",
            "usage_above",
            "rate_limited",
            "auth_failed",
            "cwd_changed",
            "schedule",
        ] {
            assert!(find(kind).is_some(), "{kind}");
        }
        assert!(find("nope").is_none());
    }
}
