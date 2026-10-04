//! The poller's wake-up: a FIFO beside the devices file. The server holds it
//! open both ways, so a write never finds it without a reader and the read
//! end never sees end-of-file; [`wake`] writes a byte without blocking, and
//! with no server running finds no reader and does nothing.

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::Result;

/// How long a wake waits for the rest of the change that caused it: a
/// command may push more than once as it finishes.
const SETTLE: Duration = Duration::from_millis(250);

fn path(devices: &Path) -> PathBuf {
    devices.with_file_name("wake")
}

/// Wake the server whose devices file is `devices`, if one runs.
pub fn wake(devices: &Path) {
    let opened = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path(devices));
    // No reader (ENXIO), no FIFO yet, or a full pipe: nothing to do.
    if let Ok(mut fifo) = opened
        && fifo.metadata().is_ok_and(|m| m.file_type().is_fifo())
    {
        let _ = fifo.write(&[1]);
    }
}

pub(super) struct Waker {
    read: File,
    write: File,
}

impl Waker {
    /// The wake-up of the server whose devices file is `devices`, made if
    /// there is none.
    pub(super) fn open(devices: &Path) -> Result<Self> {
        let path = path(devices);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_fifo()) {
            std::fs::remove_file(&path)?;
        }
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|e| std::io::Error::new(ErrorKind::InvalidInput, e))?;
        // Safety: `c_path` is a valid NUL-terminated string for the call.
        if unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != ErrorKind::AlreadyExists {
                return Err(e.into());
            }
        }
        let open = |read: bool| {
            OpenOptions::new()
                .read(read)
                .write(!read)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
        };
        let read = open(true)?;
        Ok(Self {
            read,
            write: open(false)?,
        })
    }

    /// Wait up to `timeout` for a wake. Whether one came.
    pub(super) fn wait(&self, timeout: Duration) -> bool {
        let mut fd = libc::pollfd {
            fd: self.read.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        // Safety: `fd` is one valid pollfd for the duration of the call.
        let woken = unsafe { libc::poll(&mut fd, 1, millis) } > 0;
        if woken {
            std::thread::sleep(SETTLE);
            self.drain();
        }
        woken
    }

    pub(super) fn notify(&self) {
        let _ = (&self.write).write(&[1]);
    }

    fn drain(&self) {
        let mut buf = [0; 64];
        while matches!((&self.read).read(&mut buf), Ok(n) if n > 0) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_wake_ends_the_wait_and_one_with_no_server_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let devices = dir.path().join("serve/devices.toml");
        wake(&devices);

        let waker = Waker::open(&devices).unwrap();
        assert!(!waker.wait(Duration::from_millis(50)), "nothing woke it");
        wake(&devices);
        wake(&devices);
        let start = Instant::now();
        assert!(waker.wait(Duration::from_secs(60)));
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(
            !waker.wait(Duration::from_millis(50)),
            "wakes close together are one"
        );
    }
}
