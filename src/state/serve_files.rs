//! Where `pm serve` keeps its machine-local files. The paired devices, the
//! VAPID key, the log and `state.json` are kept across runs, under the state
//! dir's `serve/`; the server's lock, the devices' lock and the wake FIFO
//! last only as long as their holders, under the runtime dir's `serve/`.
//! Both dirs are the user's alone (0700): the devices file and the key are
//! secrets.

use std::fs::{OpenOptions, TryLockError};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::dirs::Dirs;
use super::paths;
use crate::error::Result;

/// The dir name under the state and runtime dirs.
pub const DIR_NAME: &str = "serve";
pub const DEVICES: &str = "devices.toml";
pub const KEY: &str = "vapid.pem";
pub const LOG: &str = "serve.log";
pub const STATE: &str = "state.json";
pub const LOCK: &str = "serve.lock";
pub const DEVICES_LOCK: &str = "devices.lock";
pub const WAKE: &str = "wake";

/// The files' two dirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeFiles {
    /// The kept files.
    pub dir: PathBuf,
    /// The locks and the FIFO.
    pub run: PathBuf,
}

impl ServeFiles {
    pub fn new(dir: PathBuf, run: PathBuf) -> Self {
        Self { dir, run }
    }

    pub fn in_dirs(dirs: &Dirs) -> Self {
        Self::new(dirs.state.join(DIR_NAME), dirs.runtime.join(DIR_NAME))
    }

    /// The single dir an earlier release kept all of them in, under the
    /// legacy dir `legacy`.
    pub fn legacy(legacy: &Path) -> Self {
        let dir = legacy.join(DIR_NAME);
        Self::new(dir.clone(), dir)
    }

    /// This machine's.
    pub fn global() -> Result<Self> {
        let home = paths::home_dir()?;
        let dirs = paths::dirs_under(&home);
        let current = Self::in_dirs(&dirs);
        // TRANSITIONAL (drop in the release after the XDG move): a server
        // an earlier release started still holds the legacy lock and reads
        // the legacy devices, and a deferred migration leaves them there.
        let legacy = Self::legacy(&super::dirs::legacy_dir(&home, &dirs));
        Ok(Self::choose(current, legacy))
    }

    /// `legacy` while a server holds its lock or only it has been paired,
    /// else `current`.
    fn choose(current: Self, legacy: Self) -> Self {
        if legacy != current && (legacy.held() || (!current.paired() && legacy.paired())) {
            return legacy;
        }
        current
    }

    pub fn devices(&self) -> PathBuf {
        self.dir.join(DEVICES)
    }
    pub fn key(&self) -> PathBuf {
        self.dir.join(KEY)
    }
    pub fn log(&self) -> PathBuf {
        self.dir.join(LOG)
    }
    pub fn state(&self) -> PathBuf {
        self.dir.join(STATE)
    }
    pub fn lock(&self) -> PathBuf {
        self.run.join(LOCK)
    }
    pub fn devices_lock(&self) -> PathBuf {
        self.run.join(DEVICES_LOCK)
    }
    pub fn wake(&self) -> PathBuf {
        self.run.join(WAKE)
    }

    /// Make both dirs, the user's alone.
    pub fn create(&self) -> Result<()> {
        for dir in [&self.dir, &self.run] {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Whether a server holds the lock, found without making it.
    pub fn held(&self) -> bool {
        held(&self.lock())
    }

    /// Whether a devices file or a VAPID key is here.
    fn paired(&self) -> bool {
        self.devices().exists() || self.key().exists()
    }
}

/// Whether another process holds the lock file at `path`; `false` when there
/// is none, or it is not a regular file (opening a FIFO could block).
pub fn held(path: &Path) -> bool {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .is_ok_and(|file| {
            file.metadata().is_ok_and(|m| m.is_file())
                && matches!(file.try_lock(), Err(TryLockError::WouldBlock))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_held_only_while_another_handle_has_it() {
        let dir = tempfile::tempdir().unwrap();
        let files = ServeFiles::legacy(dir.path());
        assert!(!files.held(), "no file");
        files.create().unwrap();
        let lock = std::fs::File::create(files.lock()).unwrap();
        assert!(!files.held());
        lock.lock().unwrap();
        assert!(files.held());
    }

    #[test]
    fn the_legacy_files_are_used_only_while_an_old_server_or_only_they_have_them() {
        let dir = tempfile::tempdir().unwrap();
        let current = ServeFiles::new(dir.path().join("state"), dir.path().join("run"));
        let legacy = ServeFiles::legacy(&dir.path().join("legacy"));
        let choose = || ServeFiles::choose(current.clone(), legacy.clone());
        assert_eq!(choose(), current, "neither paired");

        legacy.create().unwrap();
        std::fs::write(legacy.key(), "k").unwrap();
        assert_eq!(choose(), legacy, "only the legacy dir paired");

        current.create().unwrap();
        std::fs::write(current.devices(), "").unwrap();
        assert_eq!(choose(), current, "both paired");

        let old_server = std::fs::File::create(legacy.lock()).unwrap();
        old_server.lock().unwrap();
        assert_eq!(choose(), legacy, "an old server runs");
    }

    #[test]
    fn create_makes_both_dirs_private() {
        let dir = tempfile::tempdir().unwrap();
        let files = ServeFiles::new(dir.path().join("s/serve"), dir.path().join("r/serve"));
        files.create().unwrap();
        for d in [&files.dir, &files.run] {
            let mode = std::fs::metadata(d).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }
}
