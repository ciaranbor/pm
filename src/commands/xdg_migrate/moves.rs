//! All-or-nothing moves ([`xdg_migrate`](super)).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::Result;

enum Done {
    Renamed { src: PathBuf, dst: PathBuf },
    Copied { src: PathBuf, dst: PathBuf },
}

/// Move each source to its destination, or, on any failure, put back what
/// moved and return the error.
pub(super) fn execute(plan: &[(PathBuf, PathBuf)]) -> Result<()> {
    let mut done = Vec::new();
    for (src, dst) in plan {
        match place(src, dst) {
            Ok(d) => done.push(d),
            Err(e) => {
                undo(done);
                return Err(e.into());
            }
        }
    }
    for d in done {
        if let Done::Copied { src, .. } = d {
            let _ = remove(&src);
        }
    }
    Ok(())
}

fn place(src: &Path, dst: &Path) -> std::io::Result<Done> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(Done::Renamed {
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
        }),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_into(src, dst)?;
            Ok(Done::Copied {
                src: src.to_path_buf(),
                dst: dst.to_path_buf(),
            })
        }
        Err(e) => Err(e),
    }
}

fn undo(done: Vec<Done>) {
    for d in done.into_iter().rev() {
        let _ = match d {
            Done::Renamed { src, dst } => std::fs::rename(dst, src),
            Done::Copied { dst, .. } => remove(&dst),
        };
    }
}

/// Copy `src` to `dst` on another filesystem: into a temp sibling of `dst`,
/// renamed into place once whole, so `dst` never holds half a copy.
pub(super) fn copy_into(src: &Path, dst: &Path) -> std::io::Result<()> {
    let name = dst.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dst.with_file_name(format!(".{name}.pm-migrate"));
    let _ = remove(&tmp);
    if let Err(e) = copy_tree(src, &tmp).and_then(|()| std::fs::rename(&tmp, dst)) {
        let _ = remove(&tmp);
        return Err(e);
    }
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if meta.is_dir() {
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        std::fs::set_permissions(dst, meta.permissions())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

pub(super) fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
