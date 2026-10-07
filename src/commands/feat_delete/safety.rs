//! The checks that refuse a delete that would lose work.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;

/// Safety check results for feature deletion.
pub struct SafetyReport {
    pub has_uncommitted_changes: bool,
    pub untracked_files: Vec<String>,
    pub has_unpushed_commits: bool,
    pub is_merged: bool,
}

impl SafetyReport {
    pub fn is_blocked(&self) -> bool {
        self.has_uncommitted_changes || self.has_unpushed_commits
    }

    pub fn has_warnings(&self) -> bool {
        !self.untracked_files.is_empty()
    }
}

/// Run safety checks on a feature's branch and its worktree, `None` when
/// git has none (see [`git::is_worktree`]), so there is no work in it.
/// All checks go through git.rs and propagate errors — a git failure blocks deletion.
pub fn check_safety(
    worktree: Option<&Path>,
    main_repo: &Path,
    branch: &str,
    main_branch: &str,
) -> Result<SafetyReport> {
    let (has_uncommitted_changes, untracked_files, has_unpushed_commits) = match worktree {
        Some(worktree) => (
            git::has_uncommitted_changes(worktree)?,
            git::untracked_files(worktree)?,
            git::has_unpushed_commits(worktree)?,
        ),
        None => (
            false,
            Vec::new(),
            git::branch_has_unpushed_commits(main_repo, branch)?,
        ),
    };
    let is_merged = git::branch_merged_into(main_repo, branch, main_branch)?;

    Ok(SafetyReport {
        has_uncommitted_changes,
        untracked_files,
        has_unpushed_commits,
        is_merged,
    })
}

/// Evaluate a safety report and return an error if deletion should be blocked.
/// When `pr_merged` is true, the unmerged-commits and unpushed-commits checks
/// are skipped (handles squash merges where git can't detect the merge).
pub(super) fn evaluate_safety(report: &SafetyReport, pr_merged: bool, name: &str) -> Result<()> {
    let refuse = |has: &str| {
        Err(PmError::Unsafe {
            reason: format!("feature '{name}' has {has}."),
            cli: "Use --force to override.".to_string(),
            remote: format!(
                "Deleting it anyway takes `pm feat delete --force {name}` at a terminal."
            ),
        })
    };
    if report.has_uncommitted_changes {
        return refuse("uncommitted changes");
    }
    if !report.is_merged && !pr_merged {
        return refuse("commits not merged into its base");
    }
    // Skip unpushed check when PR is merged — the commits are on GitHub already
    if report.has_unpushed_commits && !pr_merged {
        return refuse("unpushed commits");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_report(uncommitted: bool, merged: bool, unpushed: bool) -> SafetyReport {
        SafetyReport {
            has_uncommitted_changes: uncommitted,
            untracked_files: vec![],
            has_unpushed_commits: unpushed,
            is_merged: merged,
        }
    }

    #[test]
    fn safety_clean_merged_branch_passes() {
        let report = make_report(false, true, false);
        assert!(evaluate_safety(&report, false, "feat").is_ok());
    }

    #[test]
    fn safety_uncommitted_changes_always_blocks() {
        // Blocks even when git-merged
        let report = make_report(true, true, false);
        assert!(evaluate_safety(&report, false, "feat").is_err());

        // Blocks even when PR is merged
        let report = make_report(true, false, false);
        assert!(evaluate_safety(&report, true, "feat").is_err());
    }

    #[test]
    fn safety_unmerged_branch_blocks_without_pr() {
        let report = make_report(false, false, false);
        assert!(evaluate_safety(&report, false, "feat").is_err());
    }

    #[test]
    fn safety_unmerged_branch_passes_when_pr_merged() {
        let report = make_report(false, false, false);
        assert!(evaluate_safety(&report, true, "feat").is_ok());
    }

    #[test]
    fn safety_unpushed_commits_block_without_pr() {
        let report = make_report(false, true, true);
        assert!(evaluate_safety(&report, false, "feat").is_err());
    }

    #[test]
    fn safety_unpushed_commits_pass_when_pr_merged() {
        let report = make_report(false, false, true);
        assert!(evaluate_safety(&report, true, "feat").is_ok());
    }
}
