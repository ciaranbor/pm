//! How a long-running pm process (`pm tmux watch`, `pm serve`) picks up an
//! upgrade: it notes its executable as it starts and, once a different file
//! is at that path, executes it afresh with the same arguments.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::time::SystemTime;

/// The running executable, as it was when the process started.
pub struct Binary {
    path: PathBuf,
    modified: SystemTime,
}

impl Binary {
    pub fn current() -> Option<Self> {
        // A test binary rebuilt mid-run must not re-execute the suite.
        if cfg!(test) {
            return None;
        }
        let path = std::env::current_exe().ok()?;
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        Some(Self { path, modified })
    }

    /// Whether a different file is at the path now. A path with nothing
    /// there is mid-replacement, not replaced.
    pub fn replaced(&self) -> bool {
        std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .is_ok_and(|m| m != self.modified)
    }

    /// Replace this process with the binary at the path, run as this one
    /// was. Returns only on failure. Files this process opened close on the
    /// exec, locks and listening sockets with them, so the new one takes
    /// them afresh.
    pub fn exec(&self) -> std::io::Error {
        std::process::Command::new(&self.path)
            .args(std::env::args_os().skip(1))
            .exec()
    }
}
