use std::path::{Path, PathBuf};

use crate::error::Result;

/// Recursively copy a directory tree from `src` to `dst`.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Copy every file under `src` over `dst` (recursively), skipping files whose
/// bytes already match and never removing anything only in `dst`. Returns
/// `(path relative to src, replaced)` for each file written — `replaced`
/// meaning a differing file already existed there. With `dry_run` nothing is
/// written, only reported.
pub fn sync_tree(src: &Path, dst: &Path, dry_run: bool) -> Result<Vec<(PathBuf, bool)>> {
    let mut out = Vec::new();
    sync_tree_into(src, dst, Path::new(""), dry_run, &mut out)?;
    out.sort();
    Ok(out)
}

fn sync_tree_into(
    src: &Path,
    dst: &Path,
    rel: &Path,
    dry_run: bool,
    out: &mut Vec<(PathBuf, bool)>,
) -> Result<()> {
    for entry in std::fs::read_dir(src.join(rel))? {
        let entry = entry?;
        let rel_path = rel.join(entry.file_name());
        let src_path = src.join(&rel_path);
        let dst_path = dst.join(&rel_path);
        if src_path.is_dir() {
            sync_tree_into(src, dst, &rel_path, dry_run, out)?;
            continue;
        }
        let existed = dst_path.exists();
        if sync_file(&src_path, &dst_path, dry_run)? {
            out.push((rel_path, existed));
        }
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
    let tmp = parent.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    std::fs::write(&tmp, content)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}
