use super::rule::PolicyFile;
use super::validate::validate;
use super::PolicyError;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// Parse and validate policy text.
pub fn checked(text: &str) -> Result<PolicyFile, PolicyError> {
    let policy = PolicyFile::parse(text)?;
    let errors = validate(&policy);
    if errors.is_empty() {
        Ok(policy)
    } else {
        Err(PolicyError::Invalid(errors))
    }
}

/// A missing file is an empty policy.
pub fn load(path: &Path) -> Result<PolicyFile, PolicyError> {
    match fs::read_to_string(path) {
        Ok(text) => checked(&text),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(PolicyFile::default()),
        Err(e) => Err(e.into()),
    }
}

/// Validate `text`, then replace the file with it. The text is written as
/// given so the user's comments and layout survive. On any error the existing
/// file is left untouched.
pub fn save(path: &Path, text: &str) -> Result<PolicyFile, PolicyError> {
    let policy = checked(text)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const GOOD: &str = "# my rules\n[[rule]]\nname = 'rl'\non = 'rate_limited'\ndo = { swap = {} }\n";

    #[test]
    fn missing_or_empty_file_is_an_empty_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.toml");
        assert_eq!(load(&path).unwrap(), PolicyFile::default());
        fs::write(&path, "").unwrap();
        assert_eq!(load(&path).unwrap(), PolicyFile::default());
    }

    #[test]
    fn save_writes_the_text_as_given_and_load_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.toml");
        let saved = save(&path, GOOD).unwrap();
        assert_eq!(saved.rules.len(), 1);
        // Comments survive because the text is stored as the user wrote it.
        assert_eq!(fs::read_to_string(&path).unwrap(), GOOD);
        assert_eq!(load(&path).unwrap(), saved);
    }

    #[test]
    fn syntax_error_reports_a_line_and_keeps_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.toml");
        save(&path, GOOD).unwrap();
        let err = save(&path, "[[rule]]\nname = 'broken'\non = \n").unwrap_err();
        assert!(matches!(err, PolicyError::Parse(_)));
        assert!(err.to_string().contains("line 3"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), GOOD);
    }

    #[test]
    fn invalid_rule_is_rejected_and_keeps_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.toml");
        save(&path, GOOD).unwrap();
        let err = save(&path, "[[rule]]\nname = 'x'\non = 'moon_phase'\ndo = { hold = true }\n").unwrap_err();
        assert!(matches!(err, PolicyError::Invalid(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), GOOD);
        assert!(!dir.path().join("policies.toml.tmp").exists());
    }

    #[test]
    fn load_rejects_an_invalid_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.toml");
        fs::write(&path, "[[rule]]\nname = 'x'\non = 'moon_phase'\ndo = { hold = true }\n").unwrap();
        assert!(matches!(load(&path), Err(PolicyError::Invalid(_))));
    }
}
