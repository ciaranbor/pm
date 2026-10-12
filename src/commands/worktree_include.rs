//! `.worktreeinclude`: the main worktree's list, in gitignore format, of
//! ignored files a new feature worktree gets a copy of (secrets, local
//! config). It is Claude Code's format, so one file serves both tools: only
//! files git ignores are copied, as tracked ones arrive with the branch. A
//! file already in the new worktree is left as it is.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::git;
use crate::state::paths;

pub const FILE: &str = ".worktreeinclude";

/// Copy the files main's `.worktreeinclude` names into `worktree`,
/// returning what was copied, relative to both.
pub fn copy_into(project_root: &Path, worktree: &Path) -> Result<Vec<PathBuf>> {
    let main = paths::main_worktree(project_root);
    let patterns = main.join(FILE);
    if !patterns.is_file() {
        return Ok(Vec::new());
    }
    let mut copied = Vec::new();
    for rel in git::ignored_files_matching(&main, &patterns)? {
        let dst = worktree.join(&rel);
        if dst.symlink_metadata().is_ok() {
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let src = main.join(&rel);
        if src.symlink_metadata()?.file_type().is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
        copied.push(rel);
    }
    Ok(copied)
}

/// [`copy_into`], saying how many files it copied, if any.
pub fn copy_and_report(project_root: &Path, worktree: &Path) -> Result<()> {
    let copied = copy_into(project_root, worktree)?;
    if !copied.is_empty() {
        eprintln!(
            "Copied {} file(s) named in {FILE} into {}",
            copied.len(),
            worktree.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn copies_the_ignored_files_named_and_keeps_what_the_worktree_has() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let main = paths::main_worktree(root);
        git::init_repo(&main).unwrap();
        std::fs::write(main.join(".gitignore"), ".env\nconfig/local.toml\n").unwrap();
        std::fs::create_dir_all(main.join("config")).unwrap();
        std::fs::write(main.join(".env"), "SECRET=1").unwrap();
        std::fs::write(main.join("config/local.toml"), "local").unwrap();
        std::fs::write(main.join(FILE), ".env\nconfig/\n").unwrap();
        let worktree = root.join("feature");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".env"), "MINE=1").unwrap();

        let copied = copy_into(root, &worktree).unwrap();

        assert_eq!(copied, vec![PathBuf::from("config/local.toml")]);
        assert_eq!(
            std::fs::read_to_string(worktree.join("config/local.toml")).unwrap(),
            "local"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join(".env")).unwrap(),
            "MINE=1"
        );
    }
}
