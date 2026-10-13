use std::collections::HashMap;
use std::sync::Mutex;

/// Keychain service every llmpool secret is stored under.
const SERVICE: &str = "llmpool";

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// Carries the backend's message only, never a secret.
    #[error("secret store: {0}")]
    Backend(String),
}

/// Holds one secret per account id.
pub trait Vault: Send + Sync {
    fn put(&self, id: &str, secret: &str) -> Result<(), VaultError>;
    fn get(&self, id: &str) -> Result<Option<String>, VaultError>;
    /// Removing a secret that is not there is not an error.
    fn remove(&self, id: &str) -> Result<(), VaultError>;
}

/// In-process vault for tests.
#[derive(Default)]
pub struct MemoryVault(Mutex<HashMap<String, String>>);

impl Vault for MemoryVault {
    fn put(&self, id: &str, secret: &str) -> Result<(), VaultError> {
        self.0.lock().unwrap().insert(id.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<String>, VaultError> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }

    fn remove(&self, id: &str) -> Result<(), VaultError> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
}

/// The operating system's keychain.
pub struct KeychainVault;

impl KeychainVault {
    fn entry(id: &str) -> Result<keyring::Entry, VaultError> {
        keyring::Entry::new(SERVICE, id).map_err(backend)
    }
}

fn backend(e: keyring::Error) -> VaultError {
    VaultError::Backend(e.to_string())
}

impl Vault for KeychainVault {
    fn put(&self, id: &str, secret: &str) -> Result<(), VaultError> {
        Self::entry(id)?.set_password(secret).map_err(backend)
    }

    fn get(&self, id: &str) -> Result<Option<String>, VaultError> {
        match Self::entry(id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(backend(e)),
        }
    }

    fn remove(&self, id: &str) -> Result<(), VaultError> {
        match Self::entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(backend(e)),
        }
    }
}

/// Secrets as 0600 files in a directory. For tests and headless machines
/// without a keychain; selected with `LLMPOOL_FILE_VAULT=1`.
pub struct FileVault(pub std::path::PathBuf);

impl Vault for FileVault {
    fn put(&self, id: &str, secret: &str) -> Result<(), VaultError> {
        use std::io::Write;
        let io = |e: std::io::Error| VaultError::Backend(e.to_string());
        std::fs::create_dir_all(&self.0).map_err(io)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(self.0.join(id)).and_then(|mut f| f.write_all(secret.as_bytes())).map_err(io)
    }

    fn get(&self, id: &str) -> Result<Option<String>, VaultError> {
        match std::fs::read_to_string(self.0.join(id)) {
            Ok(secret) => Ok(Some(secret)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(VaultError::Backend(e.to_string())),
        }
    }

    fn remove(&self, id: &str) -> Result<(), VaultError> {
        match std::fs::remove_file(self.0.join(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(VaultError::Backend(e.to_string())),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(vault: &dyn Vault, id: &str) {
        assert_eq!(vault.get(id).unwrap(), None);
        vault.put(id, "first").unwrap();
        vault.put(id, "second").unwrap();
        assert_eq!(vault.get(id).unwrap().as_deref(), Some("second"));
        vault.remove(id).unwrap();
        vault.remove(id).unwrap();
        assert_eq!(vault.get(id).unwrap(), None);
    }

    #[test]
    fn memory_vault_stores_replaces_and_removes() {
        exercise(&MemoryVault::default(), "a1");
    }

    #[test]
    fn file_vault_stores_replaces_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        exercise(&FileVault(dir.path().join("vault")), "a1");
    }

    #[test]
    fn error_text_never_contains_the_secret() {
        let e = VaultError::Backend("no access".into());
        assert_eq!(e.to_string(), "secret store: no access");
    }

    /// Writes to the real OS keychain under a throwaway id.
    /// Run by hand: `cargo test -p llmpool-core vault -- --ignored`
    #[test]
    #[ignore]
    fn keychain_vault_stores_replaces_and_removes() {
        exercise(&KeychainVault, "llmpool-selftest");
    }
}
