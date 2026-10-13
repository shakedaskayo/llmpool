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
    /// The item's text, `None` if there is no such item, and an error if the
    /// Keychain could not be asked: a login that cannot be read must never be
    /// mistaken for a login that is not there.
    fn get(&self, service: &str) -> io::Result<Option<String>>;
    /// As `get`, for a service that keeps several items apart by account name.
    fn get_for(&self, _service: &str, _account: &str) -> io::Result<Option<String>> {
        Ok(None)
    }
    /// Store an item's text. `Ok(false)` means this Keychain does not take
    /// writes (or not one this large), and the caller should use the file.
    fn put(&self, _service: &str, _text: &str) -> io::Result<bool> {
        Ok(false)
    }
    fn delete(&self, service: &str);
}

/// No Keychain: Linux, Windows and tests. Profiles use their file only.
pub struct NoKeychain;

impl Keychain for NoKeychain {
    fn get(&self, _service: &str) -> io::Result<Option<String>> {
        Ok(None)
    }
    fn delete(&self, _service: &str) {}
}

/// The login Keychain through the `security` tool.
pub struct MacKeychain;

/// `security`'s exit code for "no such item".
const NOT_FOUND: i32 = 44;
/// `security -i` reads each command into a buffer of about this size.
const MAX_LINE: usize = 4000;
/// A locked Keychain asks the user; nothing here may wait on that forever.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

impl MacKeychain {
    fn find(args: &[&str]) -> io::Result<Option<String>> {
        use std::io::Read;
        let mut child = std::process::Command::new("/usr/bin/security")
            .arg("find-generic-password")
            .args(args)
            .arg("-w")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let start = std::time::Instant::now();
        let status = loop {
            match child.try_wait()? {
                Some(status) => break status,
                None if start.elapsed() > PATIENCE => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the Keychain did not answer; unlock it and try again",
                    ));
                }
                None => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        };
        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_string(&mut out);
        }
        match status.code() {
            Some(0) => Ok(Some(out.trim().to_string())),
            Some(NOT_FOUND) => Ok(None),
            code => Err(io::Error::other(format!(
                "the Keychain refused to give Claude Code's login (code {code:?})"
            ))),
        }
    }
}

impl Keychain for MacKeychain {
    fn get(&self, service: &str) -> io::Result<Option<String>> {
        Self::find(&["-s", service])
    }

    fn get_for(&self, service: &str, account: &str) -> io::Result<Option<String>> {
        Self::find(&["-s", service, "-a", account])
    }

    /// Written the way Claude Code writes it: through `security`, under the
    /// user's own name, the text on standard input so it is never an argument.
    fn put(&self, service: &str, text: &str) -> io::Result<bool> {
        use std::io::Write;
        let user = std::env::var("USER").unwrap_or_default();
        let plain = |t: &str| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c))
        };
        let hex: String = text.bytes().map(|b| format!("{b:02x}")).collect();
        let line = format!("add-generic-password -U -a \"{user}\" -s \"{service}\" -X {hex}\n");
        // `security -i` reads a command into a fixed buffer; a credential with
        // many MCP logins can outgrow it, and then the file is the safe place.
        if !plain(&user) || !plain(service) || line.len() > MAX_LINE {
            return Ok(false);
        }
        let mut child = std::process::Command::new("/usr/bin/security")
            .arg("-i")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(line.as_bytes());
        }
        let start = std::time::Instant::now();
        while child.try_wait()?.is_none() {
            if start.elapsed() > PATIENCE {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the Keychain did not answer; unlock it and try again",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Interactive mode reports nothing; only reading it back tells.
        Ok(self.get(service)?.as_deref() == Some(text))
    }

    fn delete(&self, service: &str) {
        let _ = std::process::Command::new("/usr/bin/security")
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
    options
        .open(&tmp)?
        .write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
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
    fn raw_credentials(&self, keychain: &dyn Keychain) -> io::Result<Option<Value>> {
        let unreadable = |what: &str| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Claude Code's login in {what} is not readable"),
            )
        };
        if let Some(text) = keychain.get(&self.service)? {
            return serde_json::from_str(&text)
                .map(Some)
                .map_err(|_| unreadable("the Keychain"));
        }
        match std::fs::read_to_string(self.credentials_path()) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|_| unreadable("its credentials file")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The login currently in this profile, if it is complete. An error means
    /// there may be a login that could not be read, which callers must not
    /// treat as "nothing here".
    pub fn read(&self, keychain: &dyn Keychain) -> io::Result<Option<Secret>> {
        let Some(oauth) = self
            .raw_credentials(keychain)?
            .and_then(|raw| raw.get("claudeAiOauth").cloned())
        else {
            return Ok(None);
        };
        let Some(account) =
            read_json(&self.global_config).and_then(|config| config.get("oauthAccount").cloned())
        else {
            return Ok(None);
        };
        Ok(Some(Secret { oauth, account }).filter(|s| s.email().is_some()))
    }

    /// (email, organization id) of the account this profile is logged in as.
    pub fn identity(&self) -> Option<(String, Option<String>)> {
        let account = read_json(&self.global_config)?.get("oauthAccount")?.clone();
        let email = account.get("emailAddress")?.as_str()?.to_string();
        let organization = account
            .get("organizationUuid")
            .and_then(Value::as_str)
            .map(str::to_string);
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

        let mut credentials = object_or_empty(self.raw_credentials(keychain)?);
        credentials["claudeAiOauth"] = secret.oauth.clone();
        let before = (
            keychain.get(&self.service)?,
            std::fs::read(self.credentials_path()).ok(),
        );

        // The login goes where Claude Code itself keeps it: the Keychain when
        // there is one, the file otherwise. Never both, or one shadows the other.
        let in_keychain = keychain.put(&self.service, &serde_json::to_string(&credentials)?)?;
        if in_keychain {
            let _ = std::fs::remove_file(self.credentials_path());
        } else {
            write_private(&self.credentials_path(), &credentials)?;
            keychain.delete(&self.service);
        }
        if let Err(e) = self.write_identity(secret) {
            // One account's token beside another's identity is worse than no
            // change at all: put the login back exactly as it was.
            match &before.0 {
                Some(text) => {
                    keychain.put(&self.service, text)?;
                }
                None => keychain.delete(&self.service),
            }
            match before.1 {
                Some(bytes) => std::fs::write(self.credentials_path(), bytes)?,
                None => {
                    let _ = std::fs::remove_file(self.credentials_path());
                }
            }
            return Err(e);
        }
        Ok(())
    }

    fn write_identity(&self, secret: &Secret) -> io::Result<()> {
        // Claude Code rewrites this file too; without its lock the two writes
        // could interleave and drop one side's change.
        let _lock = crate::lock::config(&self.global_config, std::time::Duration::from_secs(5))?;
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
        fn get(&self, service: &str) -> io::Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(service).cloned())
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
        assert_eq!(profile.read(&NoKeychain).unwrap(), None);
        profile.write(&NoKeychain, &secret("a@x.io", "t1")).unwrap();
        assert_eq!(
            profile.read(&NoKeychain).unwrap(),
            Some(secret("a@x.io", "t1"))
        );
        assert_eq!(
            profile.identity(),
            Some(("a@x.io".to_string(), Some("org-1".to_string())))
        );

        let config = read_json(&profile.global_config).unwrap();
        assert_eq!(config["hasCompletedOnboarding"], true);
        assert_eq!(config["theme"], "dark");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(profile.credentials_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn write_keeps_other_keys_and_clears_a_shadowing_keychain_item() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::default_in(dir.path());
        std::fs::create_dir_all(&profile.config_dir).unwrap();
        std::fs::write(
            &profile.global_config,
            r#"{"projects": {"/w": {}}, "oauthAccount": {"emailAddress": "old@x.io"}}"#,
        )
        .unwrap();
        let keychain = FakeKeychain::default();
        keychain.0.lock().unwrap().insert(
            profile.service.clone(),
            r#"{"claudeAiOauth": {"accessToken": "old"}, "mcpOAuth": {"server": "kept"}}"#.into(),
        );
        assert_eq!(
            profile.read(&keychain).unwrap().unwrap().oauth["accessToken"],
            "old"
        );

        profile.write(&keychain, &secret("new@x.io", "t2")).unwrap();

        assert!(keychain.get(&profile.service).unwrap().is_none());
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
        profile
            .write(&NoKeychain, &secret("a@x.io", "a-1"))
            .unwrap();
        let before = std::fs::read(profile.credentials_path()).unwrap();
        // Make the identity file impossible to replace.
        std::fs::remove_file(&profile.global_config).unwrap();
        std::fs::create_dir(&profile.global_config).unwrap();

        assert!(profile
            .write(&NoKeychain, &secret("b@x.io", "b-1"))
            .is_err());
        assert_eq!(std::fs::read(profile.credentials_path()).unwrap(), before);
    }

    /// A Keychain that cannot be asked, as when it is locked.
    struct LockedKeychain;

    impl Keychain for LockedKeychain {
        fn get(&self, _service: &str) -> io::Result<Option<String>> {
            Err(io::Error::other("locked"))
        }
        fn delete(&self, _service: &str) {}
    }

    #[test]
    fn a_login_that_cannot_be_read_is_an_error_not_an_absence() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::at(&dir.path().join("p"));
        profile
            .write(&NoKeychain, &secret("a@x.io", "a-1"))
            .unwrap();
        assert!(profile.read(&LockedKeychain).is_err());
        // And nothing is written over a login that could not be read first.
        assert!(profile
            .write(&LockedKeychain, &secret("b@x.io", "b-1"))
            .is_err());
        assert_eq!(
            profile.read(&NoKeychain).unwrap(),
            Some(secret("a@x.io", "a-1"))
        );

        std::fs::write(profile.credentials_path(), "{ damaged").unwrap();
        assert!(profile.read(&NoKeychain).is_err());
    }

    /// A Keychain that takes writes, as the real one does.
    #[derive(Default)]
    struct WritableKeychain(Mutex<HashMap<String, String>>);

    impl Keychain for WritableKeychain {
        fn get(&self, service: &str) -> io::Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(service).cloned())
        }
        fn put(&self, service: &str, text: &str) -> io::Result<bool> {
            self.0
                .lock()
                .unwrap()
                .insert(service.to_string(), text.to_string());
            Ok(true)
        }
        fn delete(&self, service: &str) {
            self.0.lock().unwrap().remove(service);
        }
    }

    #[test]
    fn with_a_keychain_the_login_stays_in_the_keychain_and_no_file_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::default_in(dir.path());
        let keychain = WritableKeychain::default();
        // An earlier version left the login in a file; that must not linger.
        profile
            .write(&NoKeychain, &secret("old@x.io", "old"))
            .unwrap();
        assert!(profile.credentials_path().exists());

        profile.write(&keychain, &secret("a@x.io", "a-1")).unwrap();
        assert!(
            !profile.credentials_path().exists(),
            "a second, stale copy on disk"
        );
        assert_eq!(
            profile.read(&keychain).unwrap(),
            Some(secret("a@x.io", "a-1"))
        );

        // A write that cannot finish puts the Keychain item back as it was.
        std::fs::remove_file(&profile.global_config).unwrap();
        std::fs::create_dir(&profile.global_config).unwrap();
        assert!(profile.write(&keychain, &secret("b@x.io", "b-1")).is_err());
        let item: Value =
            serde_json::from_str(&keychain.get(&profile.service).unwrap().unwrap()).unwrap();
        assert_eq!(item["claudeAiOauth"]["accessToken"], "a-1");
    }

    #[test]
    fn half_a_login_is_no_login() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::at(dir.path());
        std::fs::write(
            profile.credentials_path(),
            r#"{"claudeAiOauth": {"accessToken": "t"}}"#,
        )
        .unwrap();
        assert_eq!(profile.read(&NoKeychain).unwrap(), None);
        std::fs::write(&profile.global_config, "not json").unwrap();
        assert_eq!(profile.read(&NoKeychain).unwrap(), None);
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
        std::fs::write(
            &default.global_config,
            r#"{"mcpServers": {"s": {}}, "oauthAccount": {"emailAddress": "d@x.io"}}"#,
        )
        .unwrap();

        let profile = Profile::at(&dir.path().join("store"));
        profile.write(&NoKeychain, &secret("a@x.io", "t")).unwrap();
        profile.link_shared(&default).unwrap();
        profile.link_shared(&default).unwrap();

        for shared in ["settings.json", "skills", "NOTES.md"] {
            assert!(
                profile
                    .config_dir
                    .join(shared)
                    .symlink_metadata()
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "{shared}"
            );
        }
        // The session's own login must not be replaced by the default one.
        assert!(!profile
            .credentials_path()
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
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
