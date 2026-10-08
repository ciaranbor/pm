//! Where `pm serve` keeps its machine-local files. The paired devices, the
//! VAPID key, the log and `state.json` are kept across runs, under the state
//! dir's `serve/`; the server's lock, the devices' lock and the wake FIFO
//! last only as long as their holders, under the runtime dir's `serve/`.
//! Both dirs are the user's alone (0700): the devices file and the key are
//! secrets.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

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

    /// This machine's.
    pub fn global() -> Result<Self> {
        Ok(Self::in_dirs(&paths::global_dirs()?))
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
