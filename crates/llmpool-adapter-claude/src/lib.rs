//! Everything that knows how Claude Code stores its login. None of this is a
//! public contract of Claude Code; it is confined here so a change there is a
//! change in one crate.

pub mod install;
pub mod lock;
pub mod oauth;
pub mod profile;
pub mod statusline;

use sha2::{Digest, Sha256};

/// First eight hex digits of SHA-256, the form Claude Code uses to derive a
/// Keychain item name from a config directory.
pub fn short_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect()
}
