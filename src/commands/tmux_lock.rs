//! The per-server lock files under `<config dir>/tmux/`, one per kind
//! (`watch`, `refresh`), named by the server's socket, which names it however
//! it was reached.
//!
//! The files go once their server does: the watcher removes its server's as
//! it ends, and a starting watcher prunes those whose socket is gone (the
//! holder writes the socket path into the file). A file is removed only by a
//! holder of its lock, so a taker checks after locking that the file it
//! holds is still the one at the path, and otherwise takes the new one.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::state::paths;

/// A held lock; released when dropped.
pub struct Lock {
    file: File,
    path: PathBuf,
}

/// The `kind` lock of the server at `socket`, waiting for it.
pub fn lock(socket: &str, kind: &str) -> Result<Lock> {
    take(&path(socket, kind)?, socket, true).map(|lock| lock.expect("a blocking take holds"))
}

/// The `kind` lock of the server at `socket`, unless another holds it.
pub fn try_lock(socket: &str, kind: &str) -> Result<Option<Lock>> {
    take(&path(socket, kind)?, socket, false)
}

/// Remove the files of the server at `socket`, which has gone; `watch` is
/// its watch lock, held.
pub fn remove(socket: &str, watch: Lock) -> Result<()> {
    let refresh = lock(socket, "refresh")?;
    for lock in [&refresh, &watch] {
        std::fs::remove_file(&lock.path)?;
    }
    Ok(())
}

/// Remove the files of every server whose socket is gone and that no
/// watcher holds.
pub fn prune() -> Result<()> {
    for entry in std::fs::read_dir(dir()?)? {
        let path = entry?.path();
        if !path.to_string_lossy().ends_with(".watch.lock") {
            continue;
        }
        let Some(mut watch) = take(&path, "", false)? else {
            continue;
        };
        let mut socket = String::new();
        watch.file.read_to_string(&mut socket)?;
        if !socket.is_empty() && !Path::new(&socket).exists() {
            remove(&socket, watch)?;
        }
    }
    Ok(())
}

fn dir() -> Result<PathBuf> {
    let dir = paths::global_config_dir()?.join("tmux");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn path(socket: &str, kind: &str) -> Result<PathBuf> {
    let name: String = socket
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(dir()?.join(format!("{name}.{kind}.lock")))
}

/// Lock the file at `path`, recording `socket` in it unless empty. `None`
/// when `wait` is off and another holds it, or the file has gone.
fn take(path: &Path, socket: &str, wait: bool) -> Result<Option<Lock>> {
    loop {
        let file = OpenOptions::new()
            .create(!socket.is_empty())
            .truncate(false)
            .read(true)
            .write(true)
            .open(path);
        let mut file = match file {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            file => file?,
        };
        if wait {
            file.lock()?;
        } else {
            match file.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(e)) => return Err(e.into()),
            }
        }
        let held = file.metadata()?;
        let current = std::fs::metadata(path);
        if !current.is_ok_and(|m| (m.dev(), m.ino()) == (held.dev(), held.ino())) {
            if socket.is_empty() {
                return Ok(None);
            }
            continue;
        }
        if !socket.is_empty() {
            file.set_len(0)?;
            file.write_all(socket.as_bytes())?;
            file.rewind()?;
        }
        return Ok(Some(Lock {
            file,
            path: path.to_path_buf(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket(name: &str) -> String {
        let dir = paths::global_config_dir().unwrap().join("sockets");
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join(format!("{name}-{}", std::process::id()));
        std::fs::write(&socket, "").unwrap();
        socket.display().to_string()
    }

    #[test]
    fn a_lock_whose_file_was_removed_is_taken_afresh() {
        let socket = socket("removed");
        let first = try_lock(&socket, "watch").unwrap().unwrap();
        let waiting = OpenOptions::new()
            .write(true)
            .open(path(&socket, "watch").unwrap())
            .unwrap();
        remove(&socket, first).unwrap();

        let _second = try_lock(&socket, "watch").unwrap().unwrap();
        waiting
            .try_lock()
            .expect("the removed file is no longer the lock");
        assert!(try_lock(&socket, "watch").unwrap().is_none());
    }

    #[test]
    fn only_a_gone_servers_unheld_files_are_pruned() {
        let live = socket("live");
        let gone = socket("gone");
        let held = socket("held");
        for socket in [&live, &gone, &held] {
            drop(lock(socket, "refresh").unwrap());
            drop(try_lock(socket, "watch").unwrap().unwrap());
        }
        let _watcher = try_lock(&held, "watch").unwrap().unwrap();
        std::fs::remove_file(&gone).unwrap();
        std::fs::remove_file(&held).unwrap();

        prune().unwrap();

        let exists =
            |socket: &str| ["watch", "refresh"].map(|kind| path(socket, kind).unwrap().exists());
        assert_eq!(exists(&live), [true, true]);
        assert_eq!(exists(&gone), [false, false]);
        assert_eq!(exists(&held), [true, true]);
    }
}
