//! The pool as one passphrase-encrypted, text-armoured file (the `age`
//! format, so the standard `age` tool can open it too).

use age::secrecy::Secret;
use llmpool_core::model::Account;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

#[derive(Serialize, Deserialize)]
pub struct Entry {
    pub account: Account,
    pub secret: String,
}

/// Shorter passphrases are refused: the file holds logins.
const MIN_PASSPHRASE: usize = 8;

pub fn seal(entries: &[Entry], passphrase: &str) -> Result<String, String> {
    if passphrase.chars().count() < MIN_PASSPHRASE {
        return Err(format!(
            "the passphrase must be at least {MIN_PASSPHRASE} characters"
        ));
    }
    let plain = serde_json::to_vec(entries).map_err(|e| e.to_string())?;
    let encryptor = age::Encryptor::with_user_passphrase(Secret::new(passphrase.to_owned()));
    let mut sealed = Vec::new();
    let armor = age::armor::ArmoredWriter::wrap_output(&mut sealed, age::armor::Format::AsciiArmor)
        .map_err(|e| e.to_string())?;
    let mut writer = encryptor.wrap_output(armor).map_err(|e| e.to_string())?;
    writer.write_all(&plain).map_err(|e| e.to_string())?;
    writer
        .finish()
        .and_then(|armor| armor.finish())
        .map_err(|e| e.to_string())?;
    String::from_utf8(sealed).map_err(|e| e.to_string())
}

pub fn open(bundle: &str, passphrase: &str) -> Result<Vec<Entry>, String> {
    let not_ours = || "this is not an llmpool export file".to_string();
    let decryptor = match age::Decryptor::new(age::armor::ArmoredReader::new(bundle.as_bytes()))
        .map_err(|_| not_ours())?
    {
        age::Decryptor::Passphrase(decryptor) => decryptor,
        _ => return Err(not_ours()),
    };
    let mut reader = decryptor
        .decrypt(&Secret::new(passphrase.to_owned()), None)
        .map_err(|_| "wrong passphrase, or the file is damaged".to_string())?;
    let mut plain = Vec::new();
    reader
        .read_to_end(&mut plain)
        .map_err(|_| "the file is damaged".to_string())?;
    serde_json::from_slice(&plain).map_err(|_| not_ours())
}
