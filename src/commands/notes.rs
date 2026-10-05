//! `pm notes`: the project's notes, one Markdown file in pm state
//! (`.pm/notes.md`), so they sit outside every worktree and travel with
//! `pm state push` like the rest of `.pm/`. The terminal edits it in
//! `$EDITOR`; `pm serve` lets a paired phone read and write it.
//!
//! The file is the only copy, so the two editors never merge anything:
//! each save names the version it started from ([`Notes::version`]) and is
//! refused if the file has changed since. A save replaces the file by
//! renaming a complete one over it, so a reader never sees half a file and
//! an editor that has it open (vim's "file changed" check) sees a new file.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::Result;
use crate::fs_utils::write_atomic;
use crate::hash::sha256_hex;
use crate::state::paths;

/// The notes as read, with the version a save must name.
#[derive(Debug, PartialEq, Eq)]
pub struct Notes {
    pub text: String,
    /// The SHA-256 of `text`, in hex.
    pub version: String,
}

impl Notes {
    fn of(text: String) -> Self {
        let version = sha256_hex(text.as_bytes());
        Self { text, version }
    }
}

/// What a save did.
#[derive(Debug, PartialEq, Eq)]
pub enum Saved {
    /// Written; the new version.
    Written(String),
    /// Refused: the notes are no longer the version the save started from.
    Changed(Notes),
}

/// Saves within this process take turns, so two cannot both pass the
/// version check before either writes.
static SAVING: Mutex<()> = Mutex::new(());

/// Edit the project's notes in the user's editor.
pub fn edit(project_root: &Path) -> Result<()> {
    crate::editor::edit(&paths::notes_path(project_root))
}

/// The project's notes; empty when there are none yet.
pub fn read(project_root: &Path) -> Result<Notes> {
    match std::fs::read_to_string(paths::notes_path(project_root)) {
        Ok(text) => Ok(Notes::of(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Notes::of(String::new())),
        Err(e) => Err(e.into()),
    }
}

/// Replace the notes with `text` if they are still at version `base`.
pub fn save(project_root: &Path, text: &str, base: &str) -> Result<Saved> {
    let _turn = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    let current = read(project_root)?;
    if current.version != base {
        return Ok(Saved::Changed(current));
    }
    write_atomic(&target(project_root), text.as_bytes())?;
    Ok(Saved::Written(sha256_hex(text.as_bytes())))
}

/// The file a save writes: the notes file, or the existing file it links
/// to, so a symlink to one stays a symlink.
fn target(project_root: &Path) -> PathBuf {
    let path = paths::notes_path(project_root);
    std::fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_save_writes_through_a_symlinked_notes_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let elsewhere = root.join("elsewhere.md");
        std::fs::write(&elsewhere, "old\n").unwrap();
        std::fs::create_dir_all(paths::pm_dir(root)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, paths::notes_path(root)).unwrap();

        let base = read(root).unwrap().version;
        save(root, "new\n", &base).unwrap();
        assert!(
            paths::notes_path(root)
                .symlink_metadata()
                .unwrap()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "new\n");
    }
}
