use std::path::{Path, PathBuf};

use crate::error::Result;

use super::run_git;

/// Add a git worktree at the given path for the given branch.
pub fn add_worktree(repo: &Path, worktree_path: &Path, branch: &str) -> Result<()> {
    run_git(
        repo,
        &["worktree", "add", &worktree_path.to_string_lossy(), branch],
    )?;
    Ok(())
}

/// Remove a git worktree.
pub fn remove_worktree(repo: &Path, worktree_path: &Path) -> Result<()> {
    run_git(
        repo,
        &["worktree", "remove", &worktree_path.to_string_lossy()],
    )?;
    Ok(())
}

/// Force-remove a git worktree (bypasses dirty check).
pub fn remove_worktree_force(repo: &Path, worktree_path: &Path) -> Result<()> {
    run_git(
        repo,
        &[
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ],
    )?;
    Ok(())
}

/// List all worktree paths for a repo.
pub fn list_worktrees(repo: &Path) -> Result<Vec<String>> {
    let output = run_git(repo, &["worktree", "list", "--porcelain"])?;
    let paths = output
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|s| s.to_string())
        .collect();
    Ok(paths)
}

/// Prune stale worktree entries (e.g. after a worktree directory is moved/deleted).
pub fn prune_worktrees(repo: &Path) -> Result<()> {
    run_git(repo, &["worktree", "prune"])?;
    Ok(())
}

/// Find the worktree path where a given branch is checked out, if any.
pub fn find_worktree_for_branch(repo: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let output = run_git(repo, &["worktree", "list", "--porcelain"])?;
    let target_ref = format!("refs/heads/{branch}");
    let mut current_path: Option<PathBuf> = None;

    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(path));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b == target_ref {
                return Ok(current_path);
            }
        } else if line.is_empty() {
            current_path = None;
        }
    }
    Ok(None)
}

/// Move a git worktree to a new path.
pub fn move_worktree(repo: &Path, old_path: &Path, new_path: &Path) -> Result<()> {
    run_git(
        repo,
        &[
            "worktree",
            "move",
            &old_path.to_string_lossy(),
            &new_path.to_string_lossy(),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{create_branch, init_repo};
    use crate::state::paths;
    use tempfile::tempdir;

    #[test]
    fn add_worktree_creates_directory() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature-login").unwrap();

        let worktree_path = dir.path().join("feature-login");
        add_worktree(&repo_path, &worktree_path, "feature-login").unwrap();

        assert!(worktree_path.exists());
        assert!(worktree_path.is_dir());
    }

    #[test]
    fn add_worktree_appears_in_list() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature-login").unwrap();

        let worktree_path = dir.path().join("feature-login");
        add_worktree(&repo_path, &worktree_path, "feature-login").unwrap();

        let worktrees = list_worktrees(&repo_path).unwrap();
        let canonical_wt = worktree_path.canonicalize().unwrap();
        assert!(
            worktrees
                .iter()
                .any(|w| Path::new(w).canonicalize().unwrap() == canonical_wt),
            "worktree {canonical_wt:?} not found in {worktrees:?}"
        );
    }

    #[test]
    fn remove_worktree_removes_directory() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature-login").unwrap();

        let worktree_path = dir.path().join("feature-login");
        add_worktree(&repo_path, &worktree_path, "feature-login").unwrap();
        assert!(worktree_path.exists());

        remove_worktree(&repo_path, &worktree_path).unwrap();
        assert!(!worktree_path.exists());
    }

    #[test]
    fn remove_worktree_removes_from_list() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature-login").unwrap();

        let worktree_path = dir.path().join("feature-login");
        add_worktree(&repo_path, &worktree_path, "feature-login").unwrap();
        remove_worktree(&repo_path, &worktree_path).unwrap();

        let worktrees = list_worktrees(&repo_path).unwrap();
        let canonical_wt = worktree_path
            .canonicalize()
            .unwrap_or(worktree_path.clone());
        assert!(
            !worktrees
                .iter()
                .any(|w| Path::new(w) == canonical_wt.as_path()),
        );
    }

    #[test]
    fn list_worktrees_includes_main() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        let worktrees = list_worktrees(&repo_path).unwrap();
        assert!(!worktrees.is_empty());
    }

    #[test]
    fn find_worktree_for_branch_returns_path() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        let found = find_worktree_for_branch(&repo_path, "feature").unwrap();
        assert!(found.is_some());
        let found = found.unwrap().canonicalize().unwrap();
        assert_eq!(found, wt_path.canonicalize().unwrap());
    }

    #[test]
    fn find_worktree_for_branch_returns_none_for_no_worktree() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        let found = find_worktree_for_branch(&repo_path, "feature").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn prune_worktrees_cleans_stale_entry() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        // Manually remove the worktree directory (simulating a move/delete)
        std::fs::remove_dir_all(&wt_path).unwrap();

        // Before prune, git still thinks the worktree exists
        let found = find_worktree_for_branch(&repo_path, "feature").unwrap();
        assert!(found.is_some());

        prune_worktrees(&repo_path).unwrap();

        // After prune, the stale entry is gone
        let found = find_worktree_for_branch(&repo_path, "feature").unwrap();
        assert!(found.is_none());
    }
}
