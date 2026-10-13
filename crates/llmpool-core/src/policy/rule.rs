use super::PolicyError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyFile {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

impl PolicyFile {
    pub fn parse(text: &str) -> Result<Self, PolicyError> {
        toml::from_str(text).map_err(|e| PolicyError::Parse(e.to_string()))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("policy types always serialize")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    pub cooldown_secs: i64,
    pub margin_points: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cooldown_secs: 300,
            margin_points: 10.0,
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub on: TriggerSpec,
    #[serde(default, skip_serializing_if = "Conditions::is_empty")]
    pub when: Conditions,
    #[serde(rename = "do")]
    pub action: ActionSpec,
}

/// `on = "session_start"` or `on = { usage_above = { percent = 90 } }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "OnRepr", into = "OnRepr")]
pub struct TriggerSpec {
    pub kind: String,
    pub params: toml::Table,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum OnRepr {
    Name(String),
    Table(BTreeMap<String, toml::Table>),
}

impl TryFrom<OnRepr> for TriggerSpec {
    type Error = String;

    fn try_from(repr: OnRepr) -> Result<Self, String> {
        match repr {
            OnRepr::Name(kind) => Ok(Self {
                kind,
                params: toml::Table::new(),
            }),
            OnRepr::Table(mut table) if table.len() == 1 => {
                let (kind, params) = table.pop_first().expect("length checked");
                Ok(Self { kind, params })
            }
            OnRepr::Table(_) => Err("`on` must name exactly one trigger".into()),
        }
    }
}

impl From<TriggerSpec> for OnRepr {
    fn from(spec: TriggerSpec) -> Self {
        if spec.params.is_empty() {
            OnRepr::Name(spec.kind)
        } else {
            OnRepr::Table([(spec.kind, spec.params)].into())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conditions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_remote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeRange>,
}

impl Conditions {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn end_of_day() -> u8 {
    24
}

/// Holds when the weekday is listed (or `days` is empty) and
/// `from_hour <= hour < to_hour`, in local time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeRange {
    #[serde(default)]
    pub days: Vec<String>,
    #[serde(default)]
    pub from_hour: u8,
    #[serde(default = "end_of_day")]
    pub to_hour: u8,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assign: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<Target>,
    #[serde(default)]
    pub notify: bool,
    #[serde(default)]
    pub hold: bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Target {
    /// Account label or id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// A tag, or "same" for the tags of the session's current account.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    pub pick: Pick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    #[default]
    MostQuota,
    SoonestReset,
    RoundRobin,
    /// The account carrying the fewest sessions, then the most quota: spreads
    /// work instead of piling it onto one login.
    LeastBusy,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[rule]]
name = "acme repos use acme seats"
on = "session_start"
when = { cwd = "~/work/acme/**" }
do = { assign = { tag = "acme", pick = "most_quota" } }

[[rule]]
name = "swap before the limit"
on = { usage_above = { window = "5h", percent = 90 } }
do = { swap = { tag = "same" }, notify = true }
"#;

    #[test]
    fn parses_both_trigger_forms() {
        let p = PolicyFile::parse(SAMPLE).unwrap();
        assert_eq!(
            p.settings,
            Settings {
                cooldown_secs: 300,
                margin_points: 10.0
            }
        );
        assert_eq!(p.rules[0].on.kind, "session_start");
        assert!(p.rules[0].on.params.is_empty());
        assert!(p.rules[0].enabled);
        assert_eq!(p.rules[0].when.cwd.as_deref(), Some("~/work/acme/**"));
        assert_eq!(
            p.rules[0].action.assign.as_ref().unwrap().pick,
            Pick::MostQuota
        );
        assert_eq!(p.rules[1].on.kind, "usage_above");
        assert_eq!(p.rules[1].on.params["percent"].as_integer(), Some(90));
        assert!(p.rules[1].action.notify);
        assert_eq!(
            p.rules[1].action.swap.as_ref().unwrap().tag.as_deref(),
            Some("same")
        );
    }

    #[test]
    fn round_trips_through_toml() {
        let p = PolicyFile::parse(SAMPLE).unwrap();
        assert_eq!(PolicyFile::parse(&p.to_toml()).unwrap(), p);
    }

    #[test]
    fn unknown_condition_key_is_an_error_with_a_line() {
        let text = "[[rule]]\nname = 'x'\non = 'session_start'\nwhen = { colour = 'red' }\ndo = { hold = true }\n";
        let err = PolicyFile::parse(text).unwrap_err().to_string();
        assert!(err.contains("line"), "{err}");
        assert!(err.contains("colour"), "{err}");
    }

    #[test]
    fn trigger_table_with_two_keys_is_an_error() {
        let text = "[[rule]]\nname = 'x'\non = { rate_limited = {}, auth_failed = {} }\ndo = { hold = true }\n";
        assert!(PolicyFile::parse(text).is_err());
    }

    #[test]
    fn empty_text_is_an_empty_policy() {
        assert_eq!(PolicyFile::parse("").unwrap(), PolicyFile::default());
    }
}
