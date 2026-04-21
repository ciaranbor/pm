use std::path::Path;

use crate::error::{PmError, Result};

use super::run_git;

/// Create a new branch from the current HEAD.
pub fn create_branch(repo: &Path, name: &str) -> Result<()> {
    create_branch_from(repo, name, "HEAD")
}

/// Create a new branch from a specific start point.
pub fn create_branch_from(repo: &Path, name: &str, start_point: &str) -> Result<()> {
    run_git(repo, &["branch", name, start_point])?;
    Ok(())
}

/// Check if a branch exists in the repo.
pub fn branch_exists(repo: &Path, name: &str) -> Result<bool> {
    let result = run_git(
        repo,
        &["rev-parse", "--verify", &format!("refs/heads/{name}")],
    );
    match result {
        Ok(_) => Ok(true),
        Err(PmError::Git(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Rename a local branch.
pub fn rename_branch(repo: &Path, old_name: &str, new_name: &str) -> Result<()> {
    run_git(repo, &["branch", "-m", old_name, new_name])?;
    Ok(())
}

/// Delete a local branch.
pub fn delete_branch(repo: &Path, name: &str) -> Result<()> {
    run_git(repo, &["branch", "-D", name])?;
    Ok(())
}

/// Get the current branch name of a repo/worktree.
pub fn current_branch(repo: &Path) -> Result<String> {
    run_git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Check if a branch is fully merged into the given target branch.
/// Uses `merge-base --is-ancestor` which handles worktree edge cases
/// and doesn't require parsing branch listings.
pub fn branch_merged_into(repo: &Path, branch: &str, target: &str) -> Result<bool> {
    let result = run_git(repo, &["merge-base", "--is-ancestor", branch, target]);
    match result {
        Ok(_) => Ok(true),
        Err(PmError::Git(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Get the remote tracking branch for a local branch (e.g. "origin/main").
/// Returns None if no upstream is configured.
pub fn tracking_branch(repo: &Path, branch: &str) -> Result<Option<String>> {
    match run_git(
        repo,
        &[
            "rev-parse",
            "--abbrev-ref",
            &format!("{branch}@{{upstream}}"),
        ],
    ) {
        Ok(tracking) => Ok(Some(tracking)),
        Err(PmError::Git(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Get the remote tracking branch for a local branch, if one exists.
/// Returns `None` if the branch has no upstream configured.
pub fn remote_tracking_branch(repo: &Path, branch: &str) -> Result<Option<String>> {
    let result = run_git(
        repo,
        &[
            "for-each-ref",
            "--format=%(upstream:short)",
            &format!("refs/heads/{branch}"),
        ],
    );
    match result {
        Ok(upstream) if upstream.is_empty() => Ok(None),
        Ok(upstream) => Ok(Some(upstream)),
        Err(PmError::Git(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Set the upstream tracking branch for the current branch.
/// Equivalent to `git branch --set-upstream-to=<upstream>`.
pub fn set_upstream(repo: &Path, upstream: &str) -> Result<()> {
    run_git(repo, &["branch", "--set-upstream-to", upstream])?;
    Ok(())
}

/// Detect the default branch of a cloned repo by reading `refs/remotes/origin/HEAD`.
/// Returns the branch name (e.g. "main", "master") or an error if no remote HEAD is set.
pub fn default_branch(repo: &Path) -> Result<String> {
    let output = run_git(repo, &["symbolic-ref", "refs/remotes/origin/HEAD"])?;
    // Output is like "refs/remotes/origin/main"
    let branch = output
        .strip_prefix("refs/remotes/origin/")
        .unwrap_or(&output);
    Ok(branch.to_string())
}

/// Branch divergence info: how many commits ahead/behind relative to a base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchDivergence {
    pub ahead: usize,
    pub behind: usize,
}

impl std::fmt::Display for BranchDivergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.ahead, self.behind) {
            (0, 0) => write!(f, "up to date"),
            (a, 0) => write!(f, "{a} commit{} ahead", if a == 1 { "" } else { "s" }),
            (0, b) => write!(f, "{b} commit{} behind", if b == 1 { "" } else { "s" }),
            (a, b) => write!(
                f,
                "{a} commit{} ahead, {b} behind",
                if a == 1 { "" } else { "s" }
            ),
        }
    }
}

/// Count how many commits `branch` is ahead of and behind `base`.
/// Uses `git rev-list --left-right --count base...branch`.
pub fn branch_divergence(repo: &Path, branch: &str, base: &str) -> Result<BranchDivergence> {
    let output = run_git(
        repo,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{base}...{branch}"),
        ],
    )?;
    let parts: Vec<&str> = output.split_whitespace().collect();
    if parts.len() != 2 {
        return Err(PmError::Git(format!(
            "unexpected rev-list output: {output}"
        )));
    }
    let behind: usize = parts[0]
        .parse()
        .map_err(|_| PmError::Git(format!("bad rev-list count: {}", parts[0])))?;
    let ahead: usize = parts[1]
        .parse()
        .map_err(|_| PmError::Git(format!("bad rev-list count: {}", parts[1])))?;
    Ok(BranchDivergence { ahead, behind })
}

/// Merge a branch into the current branch with `--no-ff` (always create a merge commit).
pub fn merge_no_ff(repo: &Path, branch: &str) -> Result<()> {
    run_git(
        repo,
        &[
            "merge",
            "--no-ff",
            branch,
            "-m",
            &format!("Merge branch '{branch}'"),
        ],
    )?;
    Ok(())
}

/// Abort an in-progress merge.
pub fn merge_abort(repo: &Path) -> Result<()> {
    run_git(repo, &["merge", "--abort"])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{
        add_worktree, init_repo,
        status::{commit, stage_file},
    };
    use crate::state::paths;
    use tempfile::tempdir;

    #[test]
    fn create_branch_shows_in_git_branch() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature-login").unwrap();

        let branches = run_git(&repo_path, &["branch"]).unwrap();
        assert!(branches.contains("feature-login"));
    }

    #[test]
    fn branch_exists_returns_true_for_existing() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "test-branch").unwrap();

        assert!(branch_exists(&repo_path, "test-branch").unwrap());
    }

    #[test]
    fn branch_exists_returns_false_for_nonexistent() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        assert!(!branch_exists(&repo_path, "nonexistent").unwrap());
    }

    #[test]
    fn delete_branch_removes_branch() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "to-delete").unwrap();
        assert!(branch_exists(&repo_path, "to-delete").unwrap());

        delete_branch(&repo_path, "to-delete").unwrap();
        assert!(!branch_exists(&repo_path, "to-delete").unwrap());
    }

    #[test]
    fn current_branch_returns_default_branch() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        let branch = current_branch(&repo_path).unwrap();
        assert_eq!(branch, "main");
    }

    #[test]
    fn current_branch_returns_checked_out_branch_in_worktree() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        assert_eq!(current_branch(&wt_path).unwrap(), "feature");
    }

    #[test]
    fn create_branch_from_branches_at_specific_commit() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        // Make a second commit on main
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        stage_file(&repo_path, "file.txt").unwrap();
        commit(&repo_path, "second commit").unwrap();

        // Create a feature branch
        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        // Add a commit on the feature branch
        std::fs::write(wt_path.join("feat.txt"), "feat").unwrap();
        stage_file(&wt_path, "feat.txt").unwrap();
        commit(&wt_path, "feature commit").unwrap();

        // Branch from "feature", not from "main"
        create_branch_from(&repo_path, "stacked", "feature").unwrap();
        let stacked_wt = dir.path().join("stacked");
        add_worktree(&repo_path, &stacked_wt, "stacked").unwrap();

        // Stacked branch should have the feature file
        assert!(stacked_wt.join("feat.txt").exists());
    }

    #[test]
    fn branch_merged_into_returns_true_for_merged_branch() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();
        // feature branch points at same commit as main, so it's "merged"
        assert!(branch_merged_into(&repo_path, "feature", "main").unwrap());
    }

    #[test]
    fn branch_merged_into_returns_false_for_unmerged_branch() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        // Add a worktree and commit on the feature branch
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();
        std::fs::write(wt_path.join("new.txt"), "content").unwrap();
        stage_file(&wt_path, "new.txt").unwrap();
        commit(&wt_path, "feature commit").unwrap();

        assert!(!branch_merged_into(&repo_path, "feature", "main").unwrap());
    }

    #[test]
    fn merge_no_ff_merges_branch() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();
        std::fs::write(wt_path.join("new.txt"), "content").unwrap();
        stage_file(&wt_path, "new.txt").unwrap();
        commit(&wt_path, "feature commit").unwrap();

        merge_no_ff(&repo_path, "feature").unwrap();

        // The file from the feature branch should now be in the main worktree
        assert!(repo_path.join("new.txt").exists());
    }

    #[test]
    fn merge_no_ff_creates_merge_commit() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();
        std::fs::write(wt_path.join("new.txt"), "content").unwrap();
        stage_file(&wt_path, "new.txt").unwrap();
        commit(&wt_path, "feature commit").unwrap();

        merge_no_ff(&repo_path, "feature").unwrap();

        // HEAD should be a merge commit (two parents)
        let output = run_git(&repo_path, &["cat-file", "-p", "HEAD"]).unwrap();
        let parent_count = output.lines().filter(|l| l.starts_with("parent ")).count();
        assert_eq!(parent_count, 2, "merge commit should have two parents");
    }

    #[test]
    fn merge_no_ff_fails_on_nonexistent_branch() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        let result = merge_no_ff(&repo_path, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn merge_no_ff_fails_on_conflict() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        // Create a file on main and commit
        std::fs::write(repo_path.join("shared.txt"), "main content").unwrap();
        stage_file(&repo_path, "shared.txt").unwrap();
        commit(&repo_path, "main change").unwrap();

        // Create feature branch from before that commit
        run_git(&repo_path, &["branch", "feature", "HEAD~1"]).unwrap();

        // Add conflicting change on feature branch via worktree
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();
        std::fs::write(wt_path.join("shared.txt"), "feature content").unwrap();
        stage_file(&wt_path, "shared.txt").unwrap();
        commit(&wt_path, "feature change").unwrap();

        let result = merge_no_ff(&repo_path, "feature");
        assert!(result.is_err());
    }

    #[test]
    fn branch_divergence_up_to_date() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        let div = branch_divergence(&repo_path, "feature", "main").unwrap();
        assert_eq!(
            div,
            BranchDivergence {
                ahead: 0,
                behind: 0
            }
        );
        assert_eq!(div.to_string(), "up to date");
    }

    #[test]
    fn branch_divergence_ahead() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        std::fs::write(wt_path.join("feat.txt"), "content").unwrap();
        stage_file(&wt_path, "feat.txt").unwrap();
        commit(&wt_path, "feature commit").unwrap();

        let div = branch_divergence(&repo_path, "feature", "main").unwrap();
        assert_eq!(
            div,
            BranchDivergence {
                ahead: 1,
                behind: 0
            }
        );
        assert_eq!(div.to_string(), "1 commit ahead");
    }

    #[test]
    fn branch_divergence_behind() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        // Add commit on main
        std::fs::write(repo_path.join("main.txt"), "content").unwrap();
        stage_file(&repo_path, "main.txt").unwrap();
        commit(&repo_path, "main commit").unwrap();

        let div = branch_divergence(&repo_path, "feature", "main").unwrap();
        assert_eq!(
            div,
            BranchDivergence {
                ahead: 0,
                behind: 1
            }
        );
        assert_eq!(div.to_string(), "1 commit behind");
    }

    #[test]
    fn branch_divergence_both() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        // Commit on feature
        std::fs::write(wt_path.join("feat.txt"), "content").unwrap();
        stage_file(&wt_path, "feat.txt").unwrap();
        commit(&wt_path, "feature commit").unwrap();

        // Commit on main
        std::fs::write(repo_path.join("main.txt"), "content").unwrap();
        stage_file(&repo_path, "main.txt").unwrap();
        commit(&repo_path, "main commit").unwrap();

        let div = branch_divergence(&repo_path, "feature", "main").unwrap();
        assert_eq!(
            div,
            BranchDivergence {
                ahead: 1,
                behind: 1
            }
        );
        assert_eq!(div.to_string(), "1 commit ahead, 1 behind");
    }

    #[test]
    fn branch_divergence_display_plurals() {
        assert_eq!(
            BranchDivergence {
                ahead: 3,
                behind: 0
            }
            .to_string(),
            "3 commits ahead"
        );
        assert_eq!(
            BranchDivergence {
                ahead: 0,
                behind: 5
            }
            .to_string(),
            "5 commits behind"
        );
        assert_eq!(
            BranchDivergence {
                ahead: 3,
                behind: 5
            }
            .to_string(),
            "3 commits ahead, 5 behind"
        );
    }
}
