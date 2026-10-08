//! The running server's own [`ServeFiles`]: the lock it holds for its life,
//! which keeps it to one per machine; `state.json`, what the holder says of
//! itself; and its log.
//!
//! Whether a server runs is whether the lock is held, never the pid in
//! `state.json`, which a server killed leaves behind.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::fs_utils::write_atomic;
use crate::state::serve_files::ServeFiles;

fn open_lock(files: &ServeFiles) -> Result<File> {
    files.create()?;
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(files.lock())?)
}

/// The server lock of `files`, released when dropped; `None` while another
/// server holds it.
pub fn lock(files: &ServeFiles) -> Result<Option<File>> {
    let file = open_lock(files)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

/// The server lock of `files`, once its holder lets go.
pub fn wait(files: &ServeFiles) -> Result<File> {
    let file = open_lock(files)?;
    file.lock()?;
    Ok(file)
}

/// Whether a server of `files` runs.
pub fn held(files: &ServeFiles) -> Result<bool> {
    Ok(lock(files)?.is_none())
}

/// What the running server says of itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub pid: u32,
    pub exe: PathBuf,
    pub started: DateTime<Utc>,
    pub port: u16,
    /// Empty when written by a pm that predates the field.
    #[serde(default)]
    pub version: String,
}

impl State {
    /// This process, serving on `port`.
    pub fn now(port: u16) -> Self {
        Self {
            pid: std::process::id(),
            exe: std::env::current_exe().unwrap_or_default(),
            started: Utc::now(),
            port,
            version: crate::version::VERSION.to_string(),
        }
    }

    /// What the last server of `files` wrote, if readable.
    pub fn load(files: &ServeFiles) -> Option<Self> {
        let text = std::fs::read_to_string(files.state()).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, files: &ServeFiles) -> Result<()> {
        files.create()?;
        write_atomic(
            &files.state(),
            serde_json::to_string_pretty(self)?.as_bytes(),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn one_server_holds_the_lock_and_another_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = ServeFiles::legacy(dir.path());
        let first = lock(&config).unwrap().expect("free");
        assert!(held(&config).unwrap());

        let (tx, rx) = std::sync::mpsc::channel();
        let waiting = config.clone();
        std::thread::spawn(move || {
            let _lock = wait(&waiting).unwrap();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        drop(first);
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }
}
