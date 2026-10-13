use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type AccountId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Oauth,
    ApiKey,
    Cloud,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ok,
    ReloginRequired,
    Quarantined,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub used_percent: f64,
    /// Epoch seconds.
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Keyed by window name: "5h", "7d", or a model name.
    pub windows: BTreeMap<String, Window>,
    pub fetched_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub provider: String,
    pub kind: Kind,
    pub label: String,
    pub email: Option<String>,
    pub organization: Option<String>,
    pub tags: BTreeSet<String>,
    pub enabled: bool,
    pub health: Health,
    pub usage: Option<Usage>,
    pub login_expires_at: Option<i64>,
}

impl Account {
    fn window(&self, name: &str) -> Option<&Window> {
        self.usage.as_ref()?.windows.get(name)
    }

    /// Percent used in a window. An account with no reading counts as unused.
    pub fn used(&self, window: &str) -> f64 {
        self.window(window).map_or(0.0, |w| w.used_percent)
    }

    pub fn reset(&self, window: &str) -> Option<i64> {
        self.window(window)?.resets_at
    }

    /// Highest percent used across all windows.
    pub fn peak_used(&self) -> f64 {
        self.usage
            .as_ref()
            .map_or(0.0, |u| u.windows.values().map(|w| w.used_percent).fold(0.0, f64::max))
    }

    pub fn usable(&self) -> bool {
        self.enabled && self.health == Health::Ok
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent: String,
    pub provider: String,
    pub cwd: String,
    pub git_remote: Option<String>,
    pub model: Option<String>,
    pub account_id: Option<AccountId>,
    /// Epoch seconds of the last swap, for the cooldown guard.
    pub last_swap_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::account;

    #[test]
    fn account_round_trips_through_json() {
        let a = account("a1", &["work"], Some(40.0));
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"kind\":\"oauth\""));
        assert_eq!(serde_json::from_str::<Account>(&json).unwrap(), a);
    }

    #[test]
    fn unknown_usage_counts_as_unused() {
        let a = account("a1", &[], None);
        assert_eq!(a.used("5h"), 0.0);
        assert_eq!(a.peak_used(), 0.0);
        assert_eq!(a.reset("7d"), None);
    }

    #[test]
    fn disabled_or_unhealthy_is_not_usable() {
        let mut a = account("a1", &[], None);
        assert!(a.usable());
        a.enabled = false;
        assert!(!a.usable());
        a.enabled = true;
        a.health = Health::ReloginRequired;
        assert!(!a.usable());
    }
}
