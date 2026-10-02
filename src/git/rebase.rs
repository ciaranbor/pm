//! Rebase state probes.

use std::path::Path;

use crate::error::Result;

use super::run_git;

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
