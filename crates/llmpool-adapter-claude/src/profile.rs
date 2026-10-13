//! A Claude Code profile: one config directory plus the place its login lives.
//!
//! The default profile is `~/.claude` with identity in `~/.claude.json`. Any
//! other profile is a directory used as `CLAUDE_CONFIG_DIR`, with identity in
//! `<dir>/.claude.json` and a Keychain item named after a hash of the path.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_SERVICE: &str = "Claude Code-credentials";

/// Entries of the default profile that follow the user into every other
/// profile, so settings, skills, plugins and history stay shared.
const SHARED: [&str; 9] = [
    "settings.json",
    "keybindings.json",
    "CLAUDE.md",
    "skills",
    "commands",
    "agents",
    "plugins",
    "projects",
    "history.jsonl",
];

/// The macOS Keychain, as far as a profile needs it.
pub trait Keychain: Send + Sync {
    fn get(&self, service: &str) -> Option<String>;
    fn delete(&self, service: &str);
}

/// No Keychain: Linux, Windows and tests. Profiles use their file only.
pub struct NoKeychain;

impl Keychain for NoKeychain {
    fn get(&self, _service: &str) -> Option<String> {
        None
    }
    fn delete(&self, _service: &str) {}
}

/// The login Keychain through the `security` tool.
pub struct MacKeychain;

impl Keychain for MacKeychain {
    fn get(&self, service: &str) -> Option<String> {
        let out = std::process::Command::new("security")
            .args(["find-generic-password", "-s", service, "-w"])
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn delete(&self, service: &str) {
        let _ = std::process::Command::new("security")
            .args(["delete-generic-password", "-s", service])
            .output();
    }
}

/// One account's login: the `claudeAiOauth` object from the credential store
/// and the `oauthAccount` identity block from the global config.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Secret {
    pub oauth: Value,
    pub account: Value,
}

impl Secret {
    pub fn email(&self) -> Option<&str> {
        self.account.get("emailAddress")?.as_str()
    }

    pub fn organization(&self) -> Option<&str> {
        self.account.get("organizationUuid")?.as_str()
    }

    pub fn organization_name(&self) -> Option<&str> {
        self.account.get("organizationName")?.as_str()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub config_dir: PathBuf,
    pub global_config: PathBuf,
    pub service: String,
    pub is_default: bool,
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn object_or_empty(value: Option<Value>) -> Value {
    value.filter(Value::is_object).unwrap_or_else(|| json!({}))
}

/// Write JSON with owner-only permissions, replacing the file atomically.
fn write_private(path: &Path, value: &Value) -> io::Result<()> {
    use std::io::Write;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".llmpool-tmp");
    let tmp = PathBuf::from(tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&tmp)?.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
    std::fs::rename(&tmp, path)
}

impl Profile {
    pub fn default_in(home: &Path) -> Self {
        Self {
            config_dir: home.join(".claude"),
            global_config: home.join(".claude.json"),
            service: DEFAULT_SERVICE.to_string(),
            is_default: true,
        }
    }

    pub fn at(dir: &Path) -> Self {
        // Claude Code hashes the exact CLAUDE_CONFIG_DIR string it is given.
        let hash = crate::short_hash(&dir.to_string_lossy());
        Self {
            config_dir: dir.to_path_buf(),
            global_config: dir.join(".claude.json"),
            service: format!("{DEFAULT_SERVICE}-{hash}"),
            is_default: false,
        }
    }

    fn credentials_path(&self) -> PathBuf {
        self.config_dir.join(".credentials.json")
    }

    /// The whole credential object. Claude Code reads the Keychain first and
    /// the file second; so do we.
    fn raw_credentials(&self, keychain: &dyn Keychain) -> Option<Value> {
        keychain
            .get(&self.service)
            .and_then(|text| serde_json::from_str(&text).ok())
            .or_else(|| read_json(&self.credentials_path()))
    }

    /// The login currently in this profile, if it is complete.
    pub fn read(&self, keychain: &dyn Keychain) -> Option<Secret> {
        let oauth = self.raw_credentials(keychain)?.get("claudeAiOauth")?.clone();
        let account = read_json(&self.global_config)?.get("oauthAccount")?.clone();
        Some(Secret { oauth, account }).filter(|s| s.email().is_some())
    }

    /// (email, organization id) of the account this profile is logged in as.
    pub fn identity(&self) -> Option<(String, Option<String>)> {
        let account = read_json(&self.global_config)?.get("oauthAccount")?.clone();
        let email = account.get("emailAddress")?.as_str()?.to_string();
        let organization = account.get("organizationUuid").and_then(Value::as_str).map(str::to_string);
        Some((email, organization))
    }

    /// Put a login into this profile. Only the account's own keys are
    /// replaced; other content of both files (MCP logins, projects) is kept.
    pub fn write(&self, keychain: &dyn Keychain, secret: &Secret) -> io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        #[cfg(unix)]
        if !self.is_default {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.config_dir, std::fs::Permissions::from_mode(0o700))?;
        }

        let mut credentials = object_or_empty(self.raw_credentials(keychain));
        credentials["claudeAiOauth"] = secret.oauth.clone();
        let previous = std::fs::read(self.credentials_path()).ok();
        write_private(&self.credentials_path(), &credentials)?;
        if let Err(e) = self.write_identity(secret) {
            // One account's token beside another's identity is worse than no
            // change at all: put the credential file back as it was.
            match previous {
                Some(bytes) => std::fs::write(self.credentials_path(), bytes)?,
                None => std::fs::remove_file(self.credentials_path())?,
            }
            return Err(e);
        }
        // A Keychain item would shadow the file we just wrote. Claude Code
        // recreates the item itself the next time it saves this login.
        keychain.delete(&self.service);
        Ok(())
    }

    fn write_identity(&self, secret: &Secret) -> io::Result<()> {
        let mut config = object_or_empty(read_json(&self.global_config));
        config["oauthAccount"] = secret.account.clone();
        if !self.is_default {
            // Claude Code shows onboarding unless both of these are set.
            config["hasCompletedOnboarding"] = json!(true);
            if config.get("theme").is_none() {
                config["theme"] = json!("dark");
            }
        }
        write_private(&self.global_config, &config)
    }

    /// Link the default profile's shared entries into this one, and copy the
    /// user-scope MCP servers and theme. Safe to repeat.
    pub fn link_shared(&self, default: &Profile) -> io::Result<()> {
        let Ok(entries) = std::fs::read_dir(&default.config_dir) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let text = name.to_string_lossy();
            // Markdown beside CLAUDE.md is usually imported by it.
            if !(SHARED.contains(&text.as_ref()) || text.ends_with(".md")) {
                continue;
            }
            let link = self.config_dir.join(&name);
            if link.symlink_metadata().is_err() {
                #[cfg(unix)]
                std::os::unix::fs::symlink(entry.path(), &link)?;
            }
        }
        if let Some(source) = read_json(&default.global_config) {
            let mut config = object_or_empty(read_json(&self.global_config));
            for key in ["mcpServers", "theme"] {
                if let Some(value) = source.get(key) {
                    config[key] = value.clone();
                }
            }
            write_private(&self.global_config, &config)?;
        }
        Ok(())
    }

    /// Remove a non-default profile and its Keychain item. Never logs out:
    /// logging out would revoke the refresh token that was just captured.
    pub fn destroy(&self, keychain: &dyn Keychain) {
        if self.is_default {
            return;
        }
        keychain.delete(&self.service);
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeKeychain(pub Mutex<HashMap<String, String>>);

    impl Keychain for FakeKeychain {
        fn get(&self, service: &str) -> Option<String> {
            self.0.lock().unwrap().get(service).cloned()
        }
        fn delete(&self, service: &str) {
            self.0.lock().unwrap().remove(service);
        }
    }

    pub fn secret(email: &str, token: &str) -> Secret {
        Secret {
            oauth: json!({"accessToken": token, "refreshToken": format!("r-{token}")}),
            account: json!({"emailAddress": email, "organizationUuid": "org-1"}),
        }
    }

    #[test]
    fn keychain_item_name_follows_the_directory() {
        let home = Path::new("/home/u");
        assert_eq!(Profile::default_in(home).service, "Claude Code-credentials");
        let a = Profile::at(Path::new("/data/stores/a"));
        let b = Profile::at(Path::new("/data/stores/b"));
        assert!(a.service.starts_with("Claude Code-credentials-"));
        assert_eq!(a.service.len(), "Claude Code-credentials-".len() + 8);
        assert_ne!(a.service, b.service);
    }

    #[test]
    fn write_then_read_round_trips_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::at(&dir.path().join("p"));
        assert_eq!(profile.read(&NoKeychain), None);
        profile.write(&NoKeychain, &secret("a@x.io", "t1")).unwrap();
        assert_eq!(profile.read(&NoKeychain), Some(secret("a@x.io", "t1")));
        assert_eq!(profile.identity(), Some(("a@x.io".to_string(), Some("org-1".to_string()))));

        let config = read_json(&profile.global_config).unwrap();
        assert_eq!(config["hasCompletedOnboarding"], true);
        assert_eq!(config["theme"], "dark");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(profile.credentials_path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn write_keeps_other_keys_and_clears_a_shadowing_keychain_item() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::default_in(dir.path());
        std::fs::create_dir_all(&profile.config_dir).unwrap();
        std::fs::write(&profile.global_config, r#"{"projects": {"/w": {}}, "oauthAccount": {"emailAddress": "old@x.io"}}"#).unwrap();
        let keychain = FakeKeychain::default();
        keychain.0.lock().unwrap().insert(
            profile.service.clone(),
            r#"{"claudeAiOauth": {"accessToken": "old"}, "mcpOAuth": {"server": "kept"}}"#.into(),
        );
        assert_eq!(profile.read(&keychain).unwrap().oauth["accessToken"], "old");

        profile.write(&keychain, &secret("new@x.io", "t2")).unwrap();

        assert!(keychain.get(&profile.service).is_none());
        let credentials = read_json(&profile.credentials_path()).unwrap();
        assert_eq!(credentials["mcpOAuth"]["server"], "kept");
        assert_eq!(credentials["claudeAiOauth"]["accessToken"], "t2");
        let config = read_json(&profile.global_config).unwrap();
        assert!(config["projects"].get("/w").is_some());
        assert_eq!(config["oauthAccount"]["emailAddress"], "new@x.io");
        // The default profile's onboarding state is the user's, not ours.
        assert!(config.get("hasCompletedOnboarding").is_none());
    }

    #[test]
    fn a_write_that_cannot_finish_leaves_the_old_login_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::at(&dir.path().join("p"));
        profile.write(&NoKeychain, &secret("a@x.io", "a-1")).unwrap();
        let before = std::fs::read(profile.credentials_path()).unwrap();
        // Make the identity file impossible to replace.
        std::fs::remove_file(&profile.global_config).unwrap();
        std::fs::create_dir(&profile.global_config).unwrap();

        assert!(profile.write(&NoKeychain, &secret("b@x.io", "b-1")).is_err());
        assert_eq!(std::fs::read(profile.credentials_path()).unwrap(), before);
    }

    #[test]
    fn half_a_login_is_no_login() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::at(dir.path());
        std::fs::write(profile.credentials_path(), r#"{"claudeAiOauth": {"accessToken": "t"}}"#).unwrap();
        assert_eq!(profile.read(&NoKeychain), None);
        std::fs::write(&profile.global_config, "not json").unwrap();
        assert_eq!(profile.read(&NoKeychain), None);
    }

    #[cfg(unix)]
    #[test]
    fn link_shared_links_settings_and_copies_mcp_servers() {
        let dir = tempfile::tempdir().unwrap();
        let default = Profile::default_in(dir.path());
        std::fs::create_dir_all(default.config_dir.join("skills")).unwrap();
        std::fs::write(default.config_dir.join("settings.json"), "{}").unwrap();
        std::fs::write(default.config_dir.join("NOTES.md"), "x").unwrap();
        std::fs::write(default.config_dir.join(".credentials.json"), "{}").unwrap();
        std::fs::write(&default.global_config, r#"{"mcpServers": {"s": {}}, "oauthAccount": {"emailAddress": "d@x.io"}}"#).unwrap();

        let profile = Profile::at(&dir.path().join("store"));
        profile.write(&NoKeychain, &secret("a@x.io", "t")).unwrap();
        profile.link_shared(&default).unwrap();
        profile.link_shared(&default).unwrap();

        for shared in ["settings.json", "skills", "NOTES.md"] {
            assert!(profile.config_dir.join(shared).symlink_metadata().unwrap().file_type().is_symlink(), "{shared}");
        }
        // The session's own login must not be replaced by the default one.
        assert!(!profile.credentials_path().symlink_metadata().unwrap().file_type().is_symlink());
        let config = read_json(&profile.global_config).unwrap();
        assert!(config["mcpServers"].get("s").is_some());
        assert_eq!(config["oauthAccount"]["emailAddress"], "a@x.io");
    }

    #[test]
    fn destroy_removes_a_store_but_never_the_default_profile() {
        let dir = tempfile::tempdir().unwrap();
        let default = Profile::default_in(dir.path());
        default.write(&NoKeychain, &secret("d@x.io", "t")).unwrap();
        default.destroy(&NoKeychain);
        assert!(default.config_dir.exists());

        let store = Profile::at(&dir.path().join("store"));
        store.write(&NoKeychain, &secret("a@x.io", "t")).unwrap();
        store.destroy(&NoKeychain);
        assert!(!store.config_dir.exists());
    }
}
