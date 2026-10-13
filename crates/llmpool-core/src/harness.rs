//! The coding agents llmpool can start: what command runs each, whose keys
//! it reads, and where it keeps a login of its own, if it has one. Described
//! as data, so an agent llmpool has never heard of is a few lines in
//! `harnesses.toml`, not a new adapter.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Harness {
    /// Also the provider name of its own logins.
    pub name: String,
    pub title: String,
    pub command: String,
    /// The provider whose key it reads from the environment, when it reads
    /// one and only one.
    #[serde(default)]
    pub provider: Option<String>,
    /// Its own login, kept in files.
    #[serde(default)]
    pub login: Option<FileLogin>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLogin {
    /// The variable that tells the agent where its files are.
    pub home_env: String,
    /// What that variable points at by default, relative to the home directory.
    pub base: String,
    /// The agent's own directory inside it, when the variable names a place
    /// shared with other programs.
    #[serde(default)]
    pub subdir: Option<String>,
    /// The files that are the login. Everything else is shared between accounts.
    pub files: Vec<String>,
    /// Arguments that make the agent sign in, e.g. `["login"]`.
    #[serde(default)]
    pub login_args: Vec<String>,
}

impl FileLogin {
    /// (what the variable points at, the agent's own directory) under `home`.
    pub fn dirs(&self, home: &Path) -> (PathBuf, PathBuf) {
        let root = home.join(&self.base);
        let inner = match &self.subdir {
            Some(sub) => root.join(sub),
            None => root.clone(),
        };
        (root, inner)
    }
}

#[derive(Default, Deserialize)]
struct File {
    #[serde(default)]
    harness: Vec<Harness>,
}

fn file_login(home_env: &str, base: &str, subdir: Option<&str>, login_args: &[&str]) -> FileLogin {
    FileLogin {
        home_env: home_env.into(),
        base: base.into(),
        subdir: subdir.map(Into::into),
        files: vec!["auth.json".into()],
        login_args: login_args.iter().map(|s| s.to_string()).collect(),
    }
}

pub fn builtin() -> Vec<Harness> {
    let h = |name: &str, title: &str, provider: Option<&str>, login| Harness {
        name: name.into(),
        title: title.into(),
        command: name.into(),
        provider: provider.map(Into::into),
        login,
    };
    vec![
        h(
            "codex",
            "Codex CLI",
            Some("openai"),
            Some(file_login("CODEX_HOME", ".codex", None, &["login"])),
        ),
        h(
            "opencode",
            "OpenCode",
            None,
            Some(file_login(
                "XDG_DATA_HOME",
                ".local/share",
                Some("opencode"),
                &["auth", "login"],
            )),
        ),
        h("gemini", "Gemini CLI", Some("gemini"), None),
        h("grok", "Grok CLI", Some("xai"), None),
        h("aider", "Aider", None, None),
    ]
}

fn sound(h: &Harness) -> Result<(), String> {
    let plain = |s: &str| !s.is_empty() && !s.contains("..") && !s.starts_with('/');
    if !crate::providers::name_ok(&h.name) || h.name == "claude" {
        return Err(format!("`{}` is not a usable harness name", h.name));
    }
    if h.command.trim().is_empty() {
        return Err(format!("{}: `command` is empty", h.name));
    }
    if let Some(login) = &h.login {
        let env_ok = login.home_env.starts_with(|c: char| c.is_ascii_uppercase())
            && login
                .home_env
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !env_ok {
            return Err(format!("{}: `home_env` is not a variable name", h.name));
        }
        let paths = std::iter::once(&login.base)
            .chain(login.subdir.iter())
            .chain(login.files.iter());
        if login.files.is_empty() || !paths.clone().all(|p| plain(p)) {
            return Err(format!(
                "{}: `base`, `subdir` and `files` are plain relative paths, and `files` is not empty",
                h.name
            ));
        }
        if login.files.iter().any(|f| f.contains('/')) {
            return Err(format!("{}: `files` are names, not paths", h.name));
        }
    }
    Ok(())
}

/// The built-in harnesses, with the user's own from `path` added or laid over
/// them by name. A missing file is no harnesses of the user's.
pub fn load(path: &Path) -> Result<Vec<Harness>, String> {
    let mut all = builtin();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(all),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let file: File = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for harness in file.harness {
        sound(&harness)?;
        all.retain(|h| h.name != harness.name);
        all.push(harness);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_sound_and_place_their_files() {
        for h in builtin() {
            sound(&h).unwrap();
        }
        let all = builtin();
        let opencode = all.iter().find(|h| h.name == "opencode").unwrap();
        let (root, inner) = opencode.login.as_ref().unwrap().dirs(Path::new("/h"));
        assert_eq!(root, Path::new("/h/.local/share"));
        assert_eq!(inner, Path::new("/h/.local/share/opencode"));
    }

    #[test]
    fn a_users_file_adds_and_overrides_and_is_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harnesses.toml");
        assert_eq!(load(&path).unwrap(), builtin());
        std::fs::write(
            &path,
            "[[harness]]\nname = 'mycli'\ntitle = 'My CLI'\ncommand = 'mycli'\nprovider = 'openrouter'\n\
             [[harness]]\nname = 'codex'\ntitle = 'Codex'\ncommand = '/opt/codex'\n\
             [harness.login]\nhome_env = 'CODEX_HOME'\nbase = '.codex'\nfiles = ['auth.json']\n",
        )
        .unwrap();
        let all = load(&path).unwrap();
        assert_eq!(all.iter().filter(|h| h.name == "codex").count(), 1);
        assert_eq!(
            all.iter().find(|h| h.name == "codex").unwrap().command,
            "/opt/codex"
        );
        assert!(all.iter().any(|h| h.name == "mycli"));

        for bad in [
            "[[harness]]\nname = 'claude'\ntitle = 'x'\ncommand = 'x'\n",
            "[[harness]]\nname = 'x'\ntitle = 'x'\ncommand = 'x'\n[harness.login]\nhome_env = 'X'\nbase = '../..'\nfiles = ['a']\n",
            "[[harness]]\nname = 'x'\ntitle = 'x'\ncommand = 'x'\n[harness.login]\nhome_env = 'X'\nbase = 'b'\nfiles = ['../a']\n",
            "[[harness]]\nname = 'x'\ntitle = 'x'\ncommand = 'x'\n[harness.login]\nhome_env = 'x y'\nbase = 'b'\nfiles = ['a']\n",
        ] {
            std::fs::write(&path, bad).unwrap();
            assert!(load(&path).is_err(), "{bad}");
        }
    }
}
