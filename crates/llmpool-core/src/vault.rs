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
        self.0
            .lock()
            .unwrap()
            .insert(id.to_string(), secret.to_string());
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

/// Ids become file names and command arguments, so they are kept to a plain
/// alphabet. An id from outside (an imported file) is checked like any other.
fn check_id(id: &str) -> Result<(), VaultError> {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-');
    if id.is_empty() || id.starts_with('.') || !id.chars().all(plain) {
        return Err(VaultError::Backend(format!(
            "`{id}` is not a valid account id"
        )));
    }
    Ok(())
}

/// The operating system's keychain.
///
/// On macOS, items are kept through the `security` tool, the way Claude Code
/// keeps its own login. An item a program creates directly is tied to that
/// exact build of the program: after every update the system would stop at a
/// permission dialog, and a background daemon waiting on a dialog nobody is
/// looking at is a hung daemon.
pub struct KeychainVault;

#[cfg(target_os = "macos")]
mod mac {
    use super::VaultError;
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// A locked keychain asks the user; nothing here may wait on that forever.
    const PATIENCE: Duration = Duration::from_secs(10);
    /// `security -i` reads commands a line at a time into a fixed buffer.
    const MAX_LINE: usize = 4000;
    const NOT_FOUND: i32 = 44;

    fn fail(message: impl Into<String>) -> VaultError {
        VaultError::Backend(message.into())
    }

    /// Run `security`, feeding `input` to it if given. Returns (exit code, stdout).
    fn security(args: &[&str], input: Option<&str>) -> Result<(i32, String), VaultError> {
        let mut child = Command::new("/usr/bin/security")
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| fail(format!("could not run the keychain tool: {e}")))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            // The secret travels on standard input, never as an argument.
            let _ = stdin.write_all(input.as_bytes());
        }
        let start = Instant::now();
        let status = loop {
            match child.try_wait().map_err(|e| fail(e.to_string()))? {
                Some(status) => break status,
                None if start.elapsed() > PATIENCE => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(fail("the keychain did not answer; unlock it and try again"));
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_string(&mut out);
        }
        Ok((status.code().unwrap_or(1), out))
    }

    // Stored as hex text: every character is then one the tool prints back
    // exactly, whatever the secret contains.
    fn to_hex(text: &str) -> String {
        text.bytes().map(|b| format!("{b:02x}")).collect()
    }

    fn from_hex(hex: &str) -> Option<String> {
        let bytes: Option<Vec<u8>> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
            .collect();
        String::from_utf8(bytes?).ok()
    }

    pub fn get(service: &str, id: &str) -> Result<Option<String>, VaultError> {
        match security(
            &["find-generic-password", "-a", id, "-s", service, "-w"],
            None,
        )? {
            (0, out) => from_hex(out.trim())
                .map(Some)
                .ok_or_else(|| fail("the stored login is damaged")),
            (NOT_FOUND, _) => Ok(None),
            (code, _) => Err(fail(format!("the keychain refused (code {code})"))),
        }
    }

    pub fn put(service: &str, id: &str, secret: &str) -> Result<(), VaultError> {
        let line = format!(
            "add-generic-password -U -a {id} -s {service} -w {}\n",
            to_hex(secret)
        );
        // shortcut: one login is about 1.3 KB (2.6 KB as hex); switch to the
        // Security framework with a signed binary if logins ever outgrow this.
        if line.len() > MAX_LINE {
            return Err(fail("this login is too large to store in the keychain"));
        }
        security(&["-i"], Some(&line))?;
        // The tool's interactive mode does not report failure; read it back.
        if get(service, id)?.as_deref() != Some(secret) {
            return Err(fail(
                "the keychain did not keep the login; unlock it and try again",
            ));
        }
        Ok(())
    }

    pub fn remove(service: &str, id: &str) -> Result<(), VaultError> {
        match security(&["delete-generic-password", "-a", id, "-s", service], None)? {
            (0, _) | (NOT_FOUND, _) => Ok(()),
            (code, _) => Err(fail(format!("the keychain refused (code {code})"))),
        }
    }
}

#[cfg(target_os = "macos")]
impl Vault for KeychainVault {
    fn put(&self, id: &str, secret: &str) -> Result<(), VaultError> {
        check_id(id)?;
        mac::put(SERVICE, id, secret)
    }

    fn get(&self, id: &str) -> Result<Option<String>, VaultError> {
        check_id(id)?;
        mac::get(SERVICE, id)
    }

    fn remove(&self, id: &str) -> Result<(), VaultError> {
        check_id(id)?;
        mac::remove(SERVICE, id)
    }
}

#[cfg(not(target_os = "macos"))]
impl KeychainVault {
    fn entry(id: &str) -> Result<keyring::Entry, VaultError> {
        check_id(id)?;
        keyring::Entry::new(SERVICE, id).map_err(backend)
    }
}

#[cfg(not(target_os = "macos"))]
fn backend(e: keyring::Error) -> VaultError {
    VaultError::Backend(e.to_string())
}

#[cfg(not(target_os = "macos"))]
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
        check_id(id)?;
        use std::io::Write;
        let io = |e: std::io::Error| VaultError::Backend(e.to_string());
        std::fs::create_dir_all(&self.0).map_err(io)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(self.0.join(id))
            .and_then(|mut f| f.write_all(secret.as_bytes()))
            .map_err(io)
    }

    fn get(&self, id: &str) -> Result<Option<String>, VaultError> {
        check_id(id)?;
        match std::fs::read_to_string(self.0.join(id)) {
            Ok(secret) => Ok(Some(secret)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(VaultError::Backend(e.to_string())),
        }
    }

    fn remove(&self, id: &str) -> Result<(), VaultError> {
        check_id(id)?;
        match std::fs::remove_file(self.0.join(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(VaultError::Backend(e.to_string()))
            }
            _ => Ok(()),
        }
    }
}

/// Another vault with everything in it encrypted under a key held elsewhere
/// (an environment variable from a secret manager, say). For a server, whose
/// vault is files on a volume: whoever gets the volume alone gets nothing.
///
/// Each secret is sealed with XChaCha20-Poly1305 under a random nonce and
/// bound to its id, so one account's secret cannot be passed off as another's.
/// A secret stored before the key was set is still read, and sealed the next
/// time it is written.
pub struct SealedVault {
    inner: Box<dyn Vault>,
    key: [u8; 32],
}

const SEALED: &str = "llmpool-sealed:1:";
/// Shorter than this is a password, not a key.
pub const MIN_VAULT_KEY: usize = 32;

impl SealedVault {
    pub fn new(inner: Box<dyn Vault>, key: &str) -> Result<Self, VaultError> {
        use sha2::Digest;
        if key.trim().len() < MIN_VAULT_KEY {
            return Err(VaultError::Backend(format!(
                "the vault key must be at least {MIN_VAULT_KEY} characters of random text"
            )));
        }
        Ok(Self {
            inner,
            key: sha2::Sha256::digest(key.trim().as_bytes()).into(),
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    text.len()
        .is_multiple_of(2)
        .then(|| {
            (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
                .collect()
        })
        .flatten()
}

impl Vault for SealedVault {
    fn put(&self, id: &str, secret: &str) -> Result<(), VaultError> {
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        let mut nonce = [0u8; 24];
        getrandom::getrandom(&mut nonce).map_err(|e| VaultError::Backend(e.to_string()))?;
        let sealed = chacha20poly1305::XChaCha20Poly1305::new(&self.key.into())
            .encrypt(
                &nonce.into(),
                Payload {
                    msg: secret.as_bytes(),
                    aad: id.as_bytes(),
                },
            )
            .map_err(|_| VaultError::Backend("could not seal the secret".into()))?;
        self.inner
            .put(id, &format!("{SEALED}{}{}", hex(&nonce), hex(&sealed)))
    }

    fn get(&self, id: &str) -> Result<Option<String>, VaultError> {
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        let Some(stored) = self.inner.get(id)? else {
            return Ok(None);
        };
        let Some(body) = stored.strip_prefix(SEALED) else {
            return Ok(Some(stored));
        };
        let wrong = || {
            VaultError::Backend(
                "a stored secret could not be opened: the vault key is not the one it was sealed with"
                    .into(),
            )
        };
        let bytes = unhex(body).filter(|b| b.len() > 24).ok_or_else(wrong)?;
        let (nonce, sealed) = bytes.split_at(24);
        let opened = chacha20poly1305::XChaCha20Poly1305::new(&self.key.into())
            .decrypt(
                chacha20poly1305::XNonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: id.as_bytes(),
                },
            )
            .map_err(|_| wrong())?;
        String::from_utf8(opened).map(Some).map_err(|_| wrong())
    }

    fn remove(&self, id: &str) -> Result<(), VaultError> {
        self.inner.remove(id)
    }
}

#[cfg(test)]
mod sealed_tests {
    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn what_is_stored_is_unreadable_without_the_key_and_bound_to_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let files = || Box::new(FileVault(dir.path().to_path_buf()));
        let vault = SealedVault::new(files(), KEY).unwrap();
        vault.put("a", "refresh-token-value").unwrap();
        assert_eq!(
            vault.get("a").unwrap().as_deref(),
            Some("refresh-token-value")
        );
        let on_disk = std::fs::read_to_string(dir.path().join("a")).unwrap();
        assert!(on_disk.starts_with(SEALED) && !on_disk.contains("refresh-token"));

        // Sealed twice, it does not look the same twice.
        vault.put("b", "refresh-token-value").unwrap();
        assert_ne!(
            on_disk[SEALED.len()..],
            std::fs::read_to_string(dir.path().join("b")).unwrap()[SEALED.len()..]
        );

        // Another key, a moved file, a changed byte: all refused, none garbled.
        let other = SealedVault::new(files(), "ffffffffffffffffffffffffffffffff").unwrap();
        assert!(other.get("a").is_err());
        std::fs::copy(dir.path().join("a"), dir.path().join("c")).unwrap();
        assert!(vault.get("c").is_err());
        let last = if on_disk.ends_with('0') { "1" } else { "0" };
        let tampered = format!("{}{last}", &on_disk[..on_disk.len() - 1]);
        std::fs::write(dir.path().join("a"), tampered).unwrap();
        assert!(vault.get("a").is_err());

        // What was there before the key was set still reads, and is sealed
        // when next written.
        FileVault(dir.path().to_path_buf())
            .put("old", "plain")
            .unwrap();
        assert_eq!(vault.get("old").unwrap().as_deref(), Some("plain"));
        vault.put("old", "plain").unwrap();
        assert!(std::fs::read_to_string(dir.path().join("old"))
            .unwrap()
            .starts_with(SEALED));
        assert!(vault.get("missing").unwrap().is_none());
        assert!(SealedVault::new(files(), "short").is_err());
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

    #[test]
    fn ids_that_could_escape_the_vault_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault(dir.path().join("vault"));
        for id in ["../escape", "a/b", "", ".hidden", "a b", "x\ny"] {
            assert!(vault.put(id, "s").is_err(), "{id:?}");
            assert!(vault.get(id).is_err(), "{id:?}");
        }
        assert!(!dir.path().join("escape").exists());
        vault.put("claude-1a2b3c4d", "s").unwrap();
        vault.put("me@x.io", "s").unwrap();
    }

    /// Writes to the real OS keychain under a throwaway id.
    /// Run by hand: `cargo test -p llmpool-core vault -- --ignored`
    #[test]
    #[ignore]
    fn keychain_vault_stores_replaces_and_removes() {
        exercise(&KeychainVault, "llmpool-selftest");
        // A real login is JSON, with quotes and the odd non-ASCII name.
        let secret = r#"{"email":"zoë@x.io","note":"a \"quoted\" 'thing' $HOME"}"#;
        KeychainVault.put("llmpool-selftest", secret).unwrap();
        assert_eq!(
            KeychainVault.get("llmpool-selftest").unwrap().as_deref(),
            Some(secret)
        );
        KeychainVault.remove("llmpool-selftest").unwrap();
    }
}
