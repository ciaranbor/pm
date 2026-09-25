//! Rebase wrappers. A failed `git rebase` leaves its state in the worktree,
//! and whether that is a conflict to hand to the user or an error is the
//! caller's call, so `rebase` reports the failure without aborting.

use std::path::Path;

use crate::error::Result;

use super::run_git;

/// `git rebase <onto>` on the branch checked out in `worktree`.
pub fn rebase(worktree: &Path, onto: &str) -> Result<()> {
    run_git(worktree, &["rebase", onto])?;
    Ok(())
}

/// Whether a rebase (either backend) is in progress in `worktree`.
pub fn rebase_in_progress(worktree: &Path) -> Result<bool> {
    for state_dir in ["rebase-merge", "rebase-apply"] {
        let path = run_git(worktree, &["rev-parse", "--git-path", state_dir])?;
        if worktree.join(path).exists() {
            return Ok(true);
        }
    }
    Ok(false)
}
