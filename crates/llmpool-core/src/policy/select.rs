use super::context::Context;
use super::rule::{Pick, Target};
use crate::model::{Account, AccountId};
use std::collections::BTreeMap;

/// Accounts a rule may move a session to.
///
/// `ceiling` is the (window, percent) of a usage-driven rule: accounts at or
/// above it are excluded. Without one, accounts with any window full are.
pub fn candidates<'a>(
    target: &Target,
    ctx: &Context<'a>,
    ceiling: Option<(&str, f64)>,
) -> Vec<&'a Account> {
    let current = ctx.current();
    ctx.accounts
        .iter()
        .filter(|a| a.usable() && a.provider == ctx.session.provider)
        .filter(|a| current.is_none_or(|c| c.id != a.id))
        .filter(|a| match (&target.account, target.tag.as_deref()) {
            (Some(name), _) => &a.label == name || &a.id == name,
            (None, Some("same")) => {
                current.is_none_or(|c| c.tags.is_empty() || !c.tags.is_disjoint(&a.tags))
            }
            (None, Some(tag)) => a.tags.contains(tag),
            (None, None) => true,
        })
        .filter(|a| match ceiling {
            Some((window, percent)) => a.used(window) < percent,
            None => a.peak_used() < 100.0,
        })
        .collect()
}

/// Choose one candidate. Ties go to the lowest id so results are stable.
pub fn pick<'a>(
    pick: Pick,
    mut candidates: Vec<&'a Account>,
    cursor: Option<&str>,
    busy: &BTreeMap<AccountId, usize>,
) -> Option<&'a Account> {
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    let load = |a: &Account| busy.get(&a.id).copied().unwrap_or(0);
    match pick {
        // Equal quota goes to the less crowded account.
        Pick::MostQuota => candidates.into_iter().min_by(|a, b| {
            a.peak_used()
                .total_cmp(&b.peak_used())
                .then(load(a).cmp(&load(b)))
        }),
        Pick::LeastBusy => candidates.into_iter().min_by(|a, b| {
            load(a)
                .cmp(&load(b))
                .then(a.peak_used().total_cmp(&b.peak_used()))
        }),
        Pick::SoonestReset => candidates
            .into_iter()
            .min_by_key(|a| a.reset("7d").unwrap_or(i64::MAX)),
        Pick::RoundRobin => cursor
            .and_then(|last| candidates.iter().find(|a| a.id.as_str() > last).copied())
            .or_else(|| candidates.first().copied()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{account, ctx, session, NOBODY};
    use crate::model::{Health, Window};

    fn target(text: &str) -> Target {
        toml::from_str(text).unwrap()
    }

    fn ids(accounts: Vec<&Account>) -> Vec<&str> {
        accounts.into_iter().map(|a| a.id.as_str()).collect()
    }

    #[test]
    fn skips_current_disabled_unhealthy_other_provider_and_full() {
        let mut disabled = account("disabled", &[], None);
        disabled.enabled = false;
        let mut dead = account("dead", &[], None);
        dead.health = Health::ReloginRequired;
        let mut other = account("other", &[], None);
        other.provider = "openai".into();
        let accounts = [
            account("current", &[], Some(95.0)),
            disabled,
            dead,
            other,
            account("full", &[], Some(100.0)),
            account("good", &[], Some(20.0)),
        ];
        let s = session("/w", Some("current"));
        assert_eq!(
            ids(candidates(&target(""), &ctx(&s, &accounts), None)),
            ["good"]
        );
    }

    #[test]
    fn account_never_polled_is_a_candidate_and_wins_most_quota() {
        let accounts = [
            account("current", &[], Some(95.0)),
            account("fresh", &[], None),
            account("used", &[], Some(30.0)),
        ];
        let s = session("/w", Some("current"));
        let c = candidates(&target(""), &ctx(&s, &accounts), Some(("5h", 90.0)));
        assert_eq!(ids(c.clone()), ["fresh", "used"]);
        assert_eq!(pick(Pick::MostQuota, c, None, &NOBODY).unwrap().id, "fresh");
    }

    #[test]
    fn target_by_tag_account_and_same() {
        let accounts = [
            account("cur", &["work", "eu"], None),
            account("w2", &["work"], None),
            account("home", &["home"], None),
        ];
        let s = session("/w", Some("cur"));
        let c = ctx(&s, &accounts);
        assert_eq!(ids(candidates(&target("tag = 'home'"), &c, None)), ["home"]);
        assert_eq!(ids(candidates(&target("account = 'w2'"), &c, None)), ["w2"]);
        assert_eq!(ids(candidates(&target("tag = 'same'"), &c, None)), ["w2"]);
        assert!(candidates(&target("tag = 'missing'"), &c, None).is_empty());
    }

    #[test]
    fn tag_same_with_untagged_current_allows_any() {
        let accounts = [account("cur", &[], None), account("x", &["work"], None)];
        let s = session("/w", Some("cur"));
        assert_eq!(
            ids(candidates(
                &target("tag = 'same'"),
                &ctx(&s, &accounts),
                None
            )),
            ["x"]
        );
    }

    #[test]
    fn ceiling_excludes_accounts_at_or_above_it() {
        let accounts = [account("a", &[], Some(90.0)), account("b", &[], Some(89.0))];
        let s = session("/w", None);
        assert_eq!(
            ids(candidates(
                &target(""),
                &ctx(&s, &accounts),
                Some(("5h", 90.0))
            )),
            ["b"]
        );
    }

    #[test]
    fn most_quota_breaks_ties_by_id() {
        let accounts = [account("b", &[], Some(10.0)), account("a", &[], Some(10.0))];
        assert_eq!(
            pick(Pick::MostQuota, accounts.iter().collect(), None, &NOBODY)
                .unwrap()
                .id,
            "a"
        );
    }

    #[test]
    fn round_robin_walks_ids_and_wraps() {
        let accounts = [
            account("c", &[], None),
            account("a", &[], None),
            account("b", &[], None),
        ];
        let all = || accounts.iter().collect::<Vec<_>>();
        assert_eq!(
            pick(Pick::RoundRobin, all(), None, &NOBODY).unwrap().id,
            "a"
        );
        assert_eq!(
            pick(Pick::RoundRobin, all(), Some("a"), &NOBODY)
                .unwrap()
                .id,
            "b"
        );
        assert_eq!(
            pick(Pick::RoundRobin, all(), Some("c"), &NOBODY)
                .unwrap()
                .id,
            "a"
        );
    }

    #[test]
    fn soonest_reset_prefers_earliest_weekly_reset() {
        let mut late = account("late", &[], Some(10.0));
        let mut soon = account("soon", &[], Some(50.0));
        let weekly = |at| Window {
            used_percent: 5.0,
            resets_at: Some(at),
        };
        late.usage
            .as_mut()
            .unwrap()
            .windows
            .insert("7d".into(), weekly(900));
        soon.usage
            .as_mut()
            .unwrap()
            .windows
            .insert("7d".into(), weekly(100));
        let accounts = [late, soon, account("unknown", &[], None)];
        assert_eq!(
            pick(Pick::SoonestReset, accounts.iter().collect(), None, &NOBODY)
                .unwrap()
                .id,
            "soon"
        );
    }

    #[test]
    fn least_busy_spreads_sessions_and_most_quota_breaks_ties_by_load() {
        let accounts = [
            account("a", &[], Some(10.0)),
            account("b", &[], Some(60.0)),
            account("c", &[], Some(10.0)),
        ];
        let all = || accounts.iter().collect::<Vec<_>>();
        let busy: BTreeMap<AccountId, usize> = [
            ("a".to_string(), 2),
            ("b".to_string(), 0),
            ("c".to_string(), 1),
        ]
        .into();
        // The emptiest seat first, even though it has less quota left.
        assert_eq!(pick(Pick::LeastBusy, all(), None, &busy).unwrap().id, "b");
        // Same quota on a and c: the one carrying fewer sessions.
        assert_eq!(pick(Pick::MostQuota, all(), None, &busy).unwrap().id, "c");
        // Nothing known about load: quota decides, then the id.
        assert_eq!(pick(Pick::LeastBusy, all(), None, &NOBODY).unwrap().id, "a");
    }

    #[test]
    fn pick_from_nothing_is_none() {
        assert!(pick(Pick::MostQuota, Vec::new(), None, &NOBODY).is_none());
    }
}
