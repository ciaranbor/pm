use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{PmError, Result};

fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", &repo.to_string_lossy()])
        .args(args)
        .output()?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(PmError::Git(stderr))
    }
}

/// Initialize a new git repository at the given path with an initial commit.
pub fn init_repo(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;

    let output = Command::new("git")
        .args(["init", &path.to_string_lossy()])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(PmError::Git(stderr));
    }

    // Create initial commit so branches can be created
    run_git(path, &["commit", "--allow-empty", "-m", "Initial commit"])?;

    Ok(())
}

/// Create a new branch from the current HEAD.
pub fn create_branch(repo: &Path, name: &str) -> Result<()> {
    run_git(repo, &["branch", name])?;
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

/// Delete a local branch.
pub fn delete_branch(repo: &Path, name: &str) -> Result<()> {
    run_git(repo, &["branch", "-D", name])?;
    Ok(())
}

/// Check if a worktree has uncommitted changes to tracked files.
pub fn has_uncommitted_changes(worktree: &Path) -> Result<bool> {
    let output = run_git(worktree, &["status", "--porcelain"])?;
    Ok(output.lines().any(|l| !l.starts_with("??")))
}

/// List untracked, non-ignored files in a worktree.
pub fn untracked_files(worktree: &Path) -> Result<Vec<String>> {
    let output = run_git(worktree, &["ls-files", "--others", "--exclude-standard"])?;
    Ok(output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect())
}

/// Check if the worktree has commits not pushed to its upstream tracking branch.
/// Returns false if there is no upstream (that case is handled by branch_merged_into).
pub fn has_unpushed_commits(worktree: &Path) -> Result<bool> {
    // Check if there's an upstream tracking branch
    let has_upstream = run_git(worktree, &["rev-parse", "--abbrev-ref", "@{upstream}"]);
    if has_upstream.is_err() {
        return Ok(false);
    }

    let output = run_git(worktree, &["rev-list", "@{upstream}..HEAD"])?;
    Ok(!output.trim().is_empty())
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

/// Add a pattern to the repo's `.git/info/exclude` (local-only ignore).
/// Works from any worktree by resolving the shared git common dir.
pub fn exclude_pattern(repo: &Path, pattern: &str) -> Result<()> {
    let common_dir = run_git(repo, &["rev-parse", "--git-common-dir"])?;
    let common_path = if Path::new(&common_dir).is_absolute() {
        PathBuf::from(&common_dir)
    } else {
        repo.join(&common_dir)
    };
    let info_dir = common_path.join("info");
    std::fs::create_dir_all(&info_dir)?;
    let exclude_path = info_dir.join("exclude");
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if !existing.lines().any(|l| l.trim() == pattern) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exclude_path)?;
        writeln!(f, "{pattern}")?;
    }
    Ok(())
}

/// Check if a path is a git repository (has .git dir or file).
pub fn is_git_repo(path: &Path) -> bool {
    let git_path = path.join(".git");
    git_path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_repo_creates_git_directory() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");

        init_repo(&repo_path).unwrap();

        assert!(repo_path.join(".git").exists());
    }

    #[test]
    fn init_repo_creates_initial_commit() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");

        init_repo(&repo_path).unwrap();

        // git log should succeed and show at least one commit
        let output = run_git(&repo_path, &["log", "--oneline"]).unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn init_repo_allows_git_status() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");

        init_repo(&repo_path).unwrap();

        let result = run_git(&repo_path, &["status"]);
        assert!(result.is_ok());
    }

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
    fn add_worktree_creates_directory() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("main");
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
        let repo_path = dir.path().join("main");
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
        let repo_path = dir.path().join("main");
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
        let repo_path = dir.path().join("main");
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
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        let worktrees = list_worktrees(&repo_path).unwrap();
        assert!(!worktrees.is_empty());
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
    fn has_uncommitted_changes_clean_repo() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        assert!(!has_uncommitted_changes(&repo_path).unwrap());
    }

    #[test]
    fn has_uncommitted_changes_with_staged_file() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        std::fs::write(repo_path.join("file.txt"), "hello").unwrap();
        run_git(&repo_path, &["add", "file.txt"]).unwrap();

        assert!(has_uncommitted_changes(&repo_path).unwrap());
    }

    #[test]
    fn has_uncommitted_changes_ignores_untracked_files() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        // Untracked file only — should not count as uncommitted changes
        std::fs::write(repo_path.join("untracked.txt"), "hello").unwrap();

        assert!(!has_uncommitted_changes(&repo_path).unwrap());
    }

    #[test]
    fn untracked_files_lists_non_ignored_files() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        std::fs::write(repo_path.join("new_file.txt"), "hello").unwrap();

        let files = untracked_files(&repo_path).unwrap();
        assert_eq!(files, vec!["new_file.txt"]);
    }

    #[test]
    fn untracked_files_empty_when_clean() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        let files = untracked_files(&repo_path).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn has_unpushed_commits_false_without_upstream() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        // No remote, no upstream — should return false
        assert!(!has_unpushed_commits(&repo_path).unwrap());
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
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        // Add a worktree and commit on the feature branch
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();
        std::fs::write(wt_path.join("new.txt"), "content").unwrap();
        run_git(&wt_path, &["add", "new.txt"]).unwrap();
        run_git(&wt_path, &["commit", "-m", "feature commit"]).unwrap();

        assert!(!branch_merged_into(&repo_path, "feature", "main").unwrap());
    }

    #[test]
    fn is_git_repo_true_for_repo() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        init_repo(&repo_path).unwrap();

        assert!(is_git_repo(&repo_path));
    }

    #[test]
    fn is_git_repo_false_for_plain_dir() {
        let dir = tempdir().unwrap();
        assert!(!is_git_repo(dir.path()));
    }
}
