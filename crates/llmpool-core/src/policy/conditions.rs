use super::context::Context;
use super::rule::Conditions;
use globset::{Glob, GlobBuilder, GlobMatcher};

pub(crate) const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Glob over free text such as a git remote or model name: `*` matches anything.
fn text_glob(pattern: &str) -> Result<GlobMatcher, String> {
    Glob::new(pattern)
        .map(|g| g.compile_matcher())
        .map_err(|e| e.to_string())
}

/// Glob over a path: `*` stops at `/`, `**` crosses directories.
fn path_glob(pattern: &str) -> Result<GlobMatcher, String> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| e.to_string())
}

fn expand_home(path: &str, home: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) => format!("{home}{rest}"),
        None => path.to_string(),
    }
}

fn cwd_matches(pattern: &str, cwd: &str, home: &str) -> bool {
    let pattern = expand_home(pattern, home);
    let cwd = expand_home(cwd, home);
    let cwd = cwd.trim_end_matches('/');
    // `dir/**` should also cover `dir` itself.
    if pattern.strip_suffix("/**") == Some(cwd) {
        return true;
    }
    path_glob(&pattern).is_ok_and(|g| g.is_match(cwd))
}

fn text_matches(pattern: &str, value: Option<&str>) -> bool {
    value.is_some_and(|v| text_glob(pattern).is_ok_and(|g| g.is_match(v)))
}

impl Conditions {
    pub fn check(&self) -> Result<(), String> {
        if let Some(p) = &self.cwd {
            path_glob(p).map_err(|e| format!("`when.cwd`: {e}"))?;
        }
        if let Some(p) = &self.git_remote {
            text_glob(p).map_err(|e| format!("`when.git_remote`: {e}"))?;
        }
        if let Some(p) = &self.model {
            text_glob(p).map_err(|e| format!("`when.model`: {e}"))?;
        }
        if let Some(t) = &self.time {
            if let Some(day) = t.days.iter().find(|d| !DAYS.contains(&d.as_str())) {
                return Err(format!(
                    "`when.time.days`: unknown day `{day}`, use mon..sun"
                ));
            }
            if t.to_hour > 24 || t.from_hour >= t.to_hour {
                return Err("`when.time`: need from_hour < to_hour <= 24".into());
            }
        }
        Ok(())
    }

    /// True when every condition that is set holds.
    pub fn hold(&self, ctx: &Context) -> bool {
        let s = ctx.session;
        self.cwd
            .as_deref()
            .is_none_or(|p| cwd_matches(p, &s.cwd, ctx.home))
            && self
                .git_remote
                .as_deref()
                .is_none_or(|p| text_matches(p, s.git_remote.as_deref()))
            && self
                .model
                .as_deref()
                .is_none_or(|p| text_matches(p, s.model.as_deref()))
            && self
                .account_tag
                .as_ref()
                .is_none_or(|tag| ctx.current().is_some_and(|a| a.tags.contains(tag)))
            && self.time.as_ref().is_none_or(|t| {
                (t.days.is_empty() || t.days.iter().any(|d| d == ctx.weekday))
                    && (t.from_hour..t.to_hour).contains(&ctx.hour)
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::fixtures::{account, ctx, session};
    use crate::policy::rule::Conditions;

    fn when(text: &str) -> Conditions {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn empty_conditions_always_hold() {
        let s = session("/anywhere", None);
        assert!(Conditions::default().hold(&ctx(&s, &[])));
    }

    #[test]
    fn cwd_glob_matches_root_trailing_slash_children_and_tilde() {
        let c = when("cwd = '~/work/acme/**'");
        for cwd in [
            "/home/u/work/acme",
            "/home/u/work/acme/",
            "/home/u/work/acme/api/src",
            "~/work/acme/api",
        ] {
            let s = session(cwd, None);
            assert!(c.hold(&ctx(&s, &[])), "{cwd} should match");
        }
        for cwd in [
            "/home/u/work/acme-old",
            "/home/u/work",
            "/elsewhere/work/acme",
        ] {
            let s = session(cwd, None);
            assert!(!c.hold(&ctx(&s, &[])), "{cwd} should not match");
        }
    }

    #[test]
    fn single_star_does_not_cross_directories() {
        let c = when("cwd = '/src/*'");
        assert!(c.hold(&ctx(&session("/src/api", None), &[])));
        assert!(!c.hold(&ctx(&session("/src/api/deep", None), &[])));
    }

    #[test]
    fn account_tag_needs_a_current_account_with_that_tag() {
        let c = when("account_tag = 'work'");
        let accounts = [account("a", &["work"], None), account("b", &["home"], None)];
        assert!(c.hold(&ctx(&session("/w", Some("a")), &accounts)));
        assert!(!c.hold(&ctx(&session("/w", Some("b")), &accounts)));
        assert!(!c.hold(&ctx(&session("/w", None), &accounts)));
    }

    #[test]
    fn git_remote_and_model_globs() {
        let mut s = session("/w", None);
        assert!(!when("git_remote = '*acme/*'").hold(&ctx(&s, &[])));
        s.git_remote = Some("git@github.com:acme/api.git".into());
        s.model = Some("claude-opus-5-5".into());
        assert!(when("git_remote = '*acme/*'").hold(&ctx(&s, &[])));
        assert!(when("model = '*opus*'").hold(&ctx(&s, &[])));
        assert!(!when("model = '*haiku*'").hold(&ctx(&s, &[])));
    }

    #[test]
    fn time_window_uses_weekday_and_hour() {
        // The fixture context is Tuesday, 10:30.
        let s = session("/w", None);
        let c = ctx(&s, &[]);
        assert!(when("time = { days = ['mon', 'tue'], from_hour = 9, to_hour = 18 }").hold(&c));
        assert!(when("time = { from_hour = 10 }").hold(&c));
        assert!(!when("time = { days = ['sat'] }").hold(&c));
        assert!(!when("time = { from_hour = 11 }").hold(&c));
        assert!(!when("time = { to_hour = 10 }").hold(&c));
    }

    #[test]
    fn all_conditions_must_hold() {
        let c = when("cwd = '/w/**'\naccount_tag = 'work'");
        let accounts = [account("a", &["home"], None)];
        assert!(!c.hold(&ctx(&session("/w/x", Some("a")), &accounts)));
    }

    #[test]
    fn check_rejects_bad_glob_day_and_hours() {
        assert!(when("cwd = '~/ok/**'").check().is_ok());
        assert!(when("cwd = '['").check().is_err());
        assert!(when("time = { days = ['funday'] }").check().is_err());
        assert!(when("time = { to_hour = 30 }").check().is_err());
        assert!(when("time = { from_hour = 18, to_hour = 9 }")
            .check()
            .is_err());
    }
}
