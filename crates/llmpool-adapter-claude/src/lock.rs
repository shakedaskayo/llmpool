//! Claude Code guards token refresh with directory locks (the npm
//! `proper-lockfile` protocol: `mkdir` is the mutex). Taking the same locks
//! while rewriting a credential keeps a swap from interleaving with a refresh.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Claude Code treats a credential lock older than this as abandoned.
const STALE: Duration = Duration::from_secs(60);

pub struct DirLock(PathBuf);

impl Drop for DirLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

// shortcut: no mtime refresh while held, because we hold for milliseconds;
// add a toucher thread if a holder ever does network I/O under the lock.
pub fn acquire(dir: &Path, timeout: Duration) -> io::Result<DirLock> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let start = Instant::now();
    loop {
        match std::fs::create_dir(dir) {
            Ok(()) => return Ok(DirLock(dir.to_path_buf())),
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
        let age = std::fs::metadata(dir)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok());
        if age.is_some_and(|a| a > STALE) {
            let _ = std::fs::remove_dir(dir);
            continue;
        }
        if start.elapsed() > timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Claude Code is refreshing this login; try again in a few seconds",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Both locks Claude Code takes around a refresh, in its order:
/// `<config>/.oauth_refresh.lock`, then the legacy `<config>.lock`.
pub fn credentials(config_dir: &Path, timeout: Duration) -> io::Result<(DirLock, DirLock)> {
    let primary = acquire(&config_dir.join(".oauth_refresh.lock"), timeout)?;
    let mut legacy = config_dir.as_os_str().to_owned();
    legacy.push(".lock");
    let legacy = acquire(Path::new(&legacy), timeout)?;
    Ok((primary, legacy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_taker_times_out_until_the_first_releases() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("cfg");
        let held = credentials(&config, Duration::from_millis(50)).unwrap();
        let err = credentials(&config, Duration::from_millis(50)).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        drop(held);
        assert!(credentials(&config, Duration::from_millis(50)).is_ok());
        assert!(!config.join(".oauth_refresh.lock").exists());
    }
}
