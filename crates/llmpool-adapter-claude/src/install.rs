//! Adds llmpool's hooks and status line to Claude Code's `settings.json`, and
//! takes exactly those back out.

use serde_json::{json, Value};
use std::io;
use std::path::Path;

/// (event, matcher, argument passed to `llmpool hook claude`).
const HOOKS: [(&str, Option<&str>, &str); 5] = [
    ("SessionStart", None, "SessionStart"),
    ("UserPromptSubmit", None, "UserPromptSubmit"),
    ("CwdChanged", None, "CwdChanged"),
    ("StopFailure", Some("rate_limit"), "StopFailure rate_limit"),
    ("StopFailure", Some("authentication_failed"), "StopFailure authentication_failed"),
];

/// Every command llmpool installs contains this, which is how they are found
/// again.
const MARK: &str = " hook claude ";
const STATUS_MARK: &str = " statusline claude";
/// Settings `env` key naming extra plugin folders Claude Code loads.
const PLUGIN_DIRS: &str = "CLAUDE_CODE_PLUGIN_DIRS";

fn quoted(exe: &Path) -> String {
    format!("'{}'", exe.to_string_lossy().replace('\'', "'\\''"))
}

fn is_ours(group: &Value) -> bool {
    group["hooks"].as_array().is_some_and(|hooks| {
        hooks.iter().any(|h| h["command"].as_str().is_some_and(|c| c.contains("llmpool") && c.contains(MARK)))
    })
}

fn status_is_ours(settings: &Value) -> bool {
    settings["statusLine"]["command"].as_str().is_some_and(|c| c.contains("llmpool") && c.contains(STATUS_MARK))
}

fn load(path: &Path) -> io::Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e),
    }
}

fn save(path: &Path, settings: &Value) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.llmpool-tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(settings)? + "\n")?;
    std::fs::rename(&tmp, path)
}

fn remove_hooks(settings: &mut Value) {
    let Some(events) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for groups in events.values_mut().filter_map(Value::as_array_mut) {
        groups.retain(|g| !is_ours(g));
    }
    events.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    if events.is_empty() {
        settings.as_object_mut().unwrap().remove("hooks");
    }
}

fn plugin_dirs(settings: &Value) -> Vec<String> {
    settings["env"][PLUGIN_DIRS].as_str().unwrap_or_default().split(':').filter(|d| !d.is_empty()).map(str::to_string).collect()
}

fn set_plugin_dirs(settings: &mut Value, dirs: Vec<String>) {
    if !dirs.is_empty() {
        settings["env"][PLUGIN_DIRS] = json!(dirs.join(":"));
    } else if let Some(env) = settings.get_mut("env").and_then(Value::as_object_mut) {
        env.remove(PLUGIN_DIRS);
        if env.is_empty() {
            settings.as_object_mut().unwrap().remove("env");
        }
    }
}

/// Install into `settings`. The user's previous status line command is kept in
/// `previous_status` so `llmpool statusline` can still run it and `uninstall`
/// can restore it. `plugin_dir` is the folder holding llmpool's Claude Code
/// mod. Returns whether the file changed. Repeating is a no-op.
pub fn install(settings: &Path, exe: &Path, previous_status: &Path, plugin_dir: &Path) -> io::Result<bool> {
    let before = load(settings)?;
    if !before.is_object() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{} is not a JSON object", settings.display())));
    }
    let mut after = before.clone();
    let exe = quoted(exe);

    remove_hooks(&mut after);
    for (event, matcher, argument) in HOOKS {
        let mut group = json!({"hooks": [{"type": "command", "command": format!("{exe}{MARK}{argument}")}]});
        if let Some(matcher) = matcher {
            group["matcher"] = json!(matcher);
        }
        let groups = &mut after["hooks"][event];
        if !groups.is_array() {
            *groups = json!([]);
        }
        groups.as_array_mut().unwrap().push(group);
    }

    if !status_is_ours(&after) {
        if let Some(previous) = after.get("statusLine") {
            std::fs::write(previous_status, serde_json::to_string_pretty(previous)?)?;
        }
    }
    after["statusLine"] = json!({"type": "command", "command": format!("{exe}{STATUS_MARK}")});

    let plugin_dir = plugin_dir.to_string_lossy().into_owned();
    let mut dirs = plugin_dirs(&after);
    if !dirs.contains(&plugin_dir) {
        dirs.push(plugin_dir);
        set_plugin_dirs(&mut after, dirs);
    }

    if after == before {
        return Ok(false);
    }
    if settings.exists() {
        std::fs::copy(settings, settings.with_extension("json.llmpool-backup"))?;
    }
    save(settings, &after)?;
    Ok(true)
}

/// Remove what `install` added and put the previous status line back.
pub fn uninstall(settings: &Path, previous_status: &Path, plugin_dir: &Path) -> io::Result<bool> {
    let before = load(settings)?;
    let mut after = before.clone();
    remove_hooks(&mut after);
    let ours = plugin_dir.to_string_lossy();
    let dirs = plugin_dirs(&after);
    if dirs.iter().any(|d| *d == ours) {
        set_plugin_dirs(&mut after, dirs.into_iter().filter(|d| *d != ours).collect());
    }
    if status_is_ours(&after) {
        let previous = std::fs::read_to_string(previous_status).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        match previous {
            Some(previous) => after["statusLine"] = previous,
            None => {
                after.as_object_mut().unwrap().remove("statusLine");
            }
        }
        let _ = std::fs::remove_file(previous_status);
    }
    if after == before {
        return Ok(false);
    }
    save(settings, &after)?;
    Ok(true)
}

/// The status line command that was in place before llmpool's, if any.
pub fn previous_status_command(previous_status: &Path) -> Option<String> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(previous_status).ok()?).ok()?;
    value["command"].as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXISTING: &str = r#"{
  "model": "opus",
  "statusLine": {"type": "command", "command": "my-status.sh"},
  "hooks": {
    "SessionStart": [{"hooks": [{"type": "command", "command": "other-tool start"}]}],
    "Stop": [{"hooks": [{"type": "command", "command": "other-tool stop"}]}]
  }
}"#;

    fn paths(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        (dir.join("settings.json"), dir.join("prev.json"))
    }

    #[test]
    fn install_adds_hooks_and_keeps_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, prev) = paths(dir.path());
        std::fs::write(&settings, EXISTING).unwrap();

        assert!(install(&settings, Path::new("/opt/it's/llmpool"), &prev, Path::new("/data/mod")).unwrap());
        let s = load(&settings).unwrap();
        assert_eq!(s["model"], "opus");
        assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 1);
        let start = s["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 2);
        assert_eq!(start[0]["hooks"][0]["command"], "other-tool start");
        assert_eq!(start[1]["hooks"][0]["command"], r"'/opt/it'\''s/llmpool' hook claude SessionStart");
        let failures = s["hooks"]["StopFailure"].as_array().unwrap();
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0]["matcher"], "rate_limit");
        assert!(s["statusLine"]["command"].as_str().unwrap().ends_with("llmpool' statusline claude"));
        assert_eq!(previous_status_command(&prev).as_deref(), Some("my-status.sh"));
        assert_eq!(s["env"]["CLAUDE_CODE_PLUGIN_DIRS"], "/data/mod");
        assert!(settings.with_extension("json.llmpool-backup").exists());
    }

    #[test]
    fn installing_twice_changes_nothing_and_keeps_the_real_previous_status() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, prev) = paths(dir.path());
        std::fs::write(&settings, EXISTING).unwrap();
        install(&settings, Path::new("/bin/llmpool"), &prev, Path::new("/data/mod")).unwrap();
        let once = std::fs::read_to_string(&settings).unwrap();
        assert!(!install(&settings, Path::new("/bin/llmpool"), &prev, Path::new("/data/mod")).unwrap());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), once);
        assert_eq!(previous_status_command(&prev).as_deref(), Some("my-status.sh"));
    }

    #[test]
    fn uninstall_restores_the_original_settings() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, prev) = paths(dir.path());
        std::fs::write(&settings, EXISTING).unwrap();
        let original = load(&settings).unwrap();
        install(&settings, Path::new("/bin/llmpool"), &prev, Path::new("/data/mod")).unwrap();
        assert!(uninstall(&settings, &prev, Path::new("/data/mod")).unwrap());
        assert_eq!(load(&settings).unwrap(), original);
        assert!(!prev.exists());
        assert!(!uninstall(&settings, &prev, Path::new("/data/mod")).unwrap());
    }

    #[test]
    fn works_without_an_existing_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, prev) = paths(dir.path());
        assert!(install(&settings, Path::new("/bin/llmpool"), &prev, Path::new("/data/mod")).unwrap());
        assert_eq!(previous_status_command(&prev), None);
        assert!(uninstall(&settings, &prev, Path::new("/data/mod")).unwrap());
        assert_eq!(load(&settings).unwrap(), json!({}));
    }

    #[test]
    fn malformed_settings_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, prev) = paths(dir.path());
        std::fs::write(&settings, "{ broken").unwrap();
        assert!(install(&settings, Path::new("/bin/llmpool"), &prev, Path::new("/data/mod")).is_err());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{ broken");
    }

    #[test]
    fn plugin_dir_joins_and_leaves_an_existing_list() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, prev) = paths(dir.path());
        std::fs::write(&settings, r#"{"env": {"CLAUDE_CODE_PLUGIN_DIRS": "/mine", "OTHER": "1"}}"#).unwrap();
        install(&settings, Path::new("/bin/llmpool"), &prev, Path::new("/data/mod")).unwrap();
        assert_eq!(load(&settings).unwrap()["env"]["CLAUDE_CODE_PLUGIN_DIRS"], "/mine:/data/mod");
        uninstall(&settings, &prev, Path::new("/data/mod")).unwrap();
        assert_eq!(load(&settings).unwrap()["env"], json!({"CLAUDE_CODE_PLUGIN_DIRS": "/mine", "OTHER": "1"}));
    }
}
