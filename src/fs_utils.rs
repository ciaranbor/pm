use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};

/// Recursively copy a directory tree from `src` to `dst`, skipping
/// [`write_atomic`]'s in-flight temp files and entries that vanish while
/// being copied.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if is_atomic_temp(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let copied = if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map(|_| ())
                .map_err(Into::into)
        };
        absent_if_vanished(copied)?;
    }
    Ok(())
}

/// `Ok` for a source entry deleted (or renamed away) between listing its
/// directory and reading it — a concurrent [`write_atomic`] does exactly
/// that to its temp file.
fn absent_if_vanished(result: Result<()>) -> Result<()> {
    match result {
        Err(PmError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Copy every file under `src` over `dst` (recursively), skipping files whose
/// bytes already match and never removing anything only in `dst` except
/// [`write_atomic`] temp files abandoned by a process that has exited.
/// In-flight temp files in `src` are not copied. Returns
/// `(path relative to src, replaced)` for each file written — `replaced`
/// meaning a differing file already existed there. With `dry_run` nothing is
/// written, only reported.
pub fn sync_tree(src: &Path, dst: &Path, dry_run: bool) -> Result<Vec<(PathBuf, bool)>> {
    sync_tree_except(src, dst, &HashSet::new(), dry_run)
}

/// [`sync_tree`], leaving alone every file in `keep` (paths relative to
/// `dst`).
pub fn sync_tree_except(
    src: &Path,
    dst: &Path,
    keep: &HashSet<PathBuf>,
    dry_run: bool,
) -> Result<Vec<(PathBuf, bool)>> {
    let mut out = Vec::new();
    sync_tree_into(src, dst, Path::new(""), keep, dry_run, &mut out)?;
    if !dry_run {
        remove_abandoned_temps(dst)?;
    }
    out.sort();
    Ok(out)
}

fn sync_tree_into(
    src: &Path,
    dst: &Path,
    rel: &Path,
    keep: &HashSet<PathBuf>,
    dry_run: bool,
    out: &mut Vec<(PathBuf, bool)>,
) -> Result<()> {
    for entry in std::fs::read_dir(src.join(rel))? {
        let entry = entry?;
        if is_atomic_temp(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let rel_path = rel.join(entry.file_name());
        let src_path = src.join(&rel_path);
        let dst_path = dst.join(&rel_path);
        if src_path.is_dir() {
            absent_if_vanished(sync_tree_into(src, dst, &rel_path, keep, dry_run, out))?;
            continue;
        }
        if keep.contains(&rel_path) {
            continue;
        }
        let existed = dst_path.exists();
        let synced = sync_file(&src_path, &dst_path, dry_run);
        if let Ok(true) = synced {
            out.push((rel_path, existed));
        }
        absent_if_vanished(synced.map(|_| ()))?;
    }
    Ok(())
}

/// Copy `src` over `dst` unless the bytes already match. Returns whether
/// `dst` was (or, with `dry_run`, would be) written.
pub fn sync_file(src: &Path, dst: &Path, dry_run: bool) -> Result<bool> {
    let content = std::fs::read(src)?;
    if dst.exists() && std::fs::read(dst)? == content {
        return Ok(false);
    }
    if !dry_run {
        write_atomic(dst, &content)?;
    }
    Ok(true)
}

const TMP_SUFFIX: &str = ".tmp";

/// The pid that wrote `name`, if it is a [`write_atomic`] temp file name
/// (`.<target>.<pid>.<n>.tmp`).
fn atomic_temp_writer(name: &str) -> Option<u32> {
    let stem = name.strip_prefix('.')?.strip_suffix(TMP_SUFFIX)?;
    let mut parts = stem.rsplitn(3, '.');
    let n = parts.next()?;
    let pid = parts.next()?;
    let target = parts.next()?;
    if target.is_empty() || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

fn is_atomic_temp(name: &str) -> bool {
    atomic_temp_writer(name).is_some()
}

/// Delete, anywhere under `dir`, [`write_atomic`] temp files whose writer
/// has exited. A live writer's file is its in-flight write and stays.
fn remove_abandoned_temps(dir: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            remove_abandoned_temps(&path)?;
        } else if atomic_temp_writer(&entry.file_name().to_string_lossy())
            .is_some_and(|pid| !process_alive(pid))
        {
            absent_if_vanished(std::fs::remove_file(&path).map_err(Into::into))?;
        }
    }
    Ok(())
}

fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // Safety: signal 0 delivers nothing; it only checks the pid.
    let sent = unsafe { libc::kill(pid, 0) };
    // EPERM is a live process owned by someone else.
    sent == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Write `content` to `path` via a uniquely-named sibling temp file and a
/// rename, creating parent directories as needed. A concurrent reader sees
/// either the old file or the complete new one, never a partial write.
pub fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{name}.{}.{n}{TMP_SUFFIX}", std::process::id()));
    std::fs::write(&tmp, content)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syncing_skips_in_flight_temps_and_removes_abandoned_ones() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let live = format!(".c.md.{}.0.tmp", std::process::id());
        std::fs::write(src.path().join("a.md"), "a").unwrap();
        std::fs::write(src.path().join(&live), "partial").unwrap();
        let dead = ".b.md.999999999.3.tmp";
        std::fs::write(dst.path().join(dead), "stale").unwrap();
        std::fs::write(dst.path().join(&live), "in flight").unwrap();
        std::fs::write(dst.path().join("notes.tmp"), "user's").unwrap();

        let written = sync_tree(src.path(), dst.path(), false).unwrap();

        assert_eq!(written, vec![(PathBuf::from("a.md"), false)]);
        assert!(!dst.path().join(dead).exists());
        assert!(dst.path().join(&live).exists());
        assert!(dst.path().join("notes.tmp").exists());

        let staged = tempfile::tempdir().unwrap();
        copy_dir_recursive(src.path(), staged.path()).unwrap();
        assert!(staged.path().join("a.md").exists());
        assert!(!staged.path().join(&live).exists());
    }
}
