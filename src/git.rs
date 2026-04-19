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

/// Clone a remote git repository into the given path.
pub fn clone_repo(url: &str, path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["clone", url, &path.to_string_lossy()])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(PmError::Git(stderr));
    }

    Ok(())
}

/// Create a new branch from the current HEAD.
pub fn create_branch(repo: &Path, name: &str) -> Result<()> {
    create_branch_from(repo, name, "HEAD")
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

/// Rename a local branch.
pub fn rename_branch(repo: &Path, old_name: &str, new_name: &str) -> Result<()> {
    run_git(repo, &["branch", "-m", old_name, new_name])?;
    Ok(())
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

/// Fetch from all remotes. Works from any worktree.
pub fn fetch(repo: &Path) -> Result<()> {
    run_git(repo, &["fetch"])?;
    Ok(())
}

/// Pull from the remote (fast-forward only).
pub fn pull(repo: &Path) -> Result<()> {
    run_git(repo, &["pull", "--ff-only"])?;
    Ok(())
}

/// Fetch from a specific remote.
pub fn fetch_remote(repo: &Path, remote: &str) -> Result<()> {
    run_git(repo, &["fetch", remote])?;
    Ok(())
}

/// Hard-reset the current branch to a given ref (e.g. `origin/main`).
pub fn reset_hard(repo: &Path, refspec: &str) -> Result<()> {
    run_git(repo, &["reset", "--hard", refspec])?;
    Ok(())
}

/// List remote tracking branches (e.g. `origin/main`).
/// Returns branch names as they appear in `git branch -r` output.
pub fn list_remote_branches(repo: &Path) -> Result<Vec<String>> {
    let output = run_git(repo, &["branch", "-r"])?;
    Ok(output
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.contains("->"))
        .collect())
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

/// Abort an in-progress merge.
pub fn merge_abort(repo: &Path) -> Result<()> {
    run_git(repo, &["merge", "--abort"])?;
    Ok(())
}

/// Stage all changes in the given repo/worktree (`git add -A`).
pub fn add_all(repo: &Path) -> Result<()> {
    run_git(repo, &["add", "-A"])?;
    Ok(())
}

/// List file names with staged changes.
pub fn staged_file_names(repo: &Path) -> Result<Vec<String>> {
    let output = run_git(repo, &["diff", "--cached", "--name-only"])?;
    Ok(output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect())
}

/// Check if there are staged changes ready to commit.
/// Returns `true` if there are staged changes.
pub fn has_staged_changes(repo: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", &repo.to_string_lossy()])
        .args(["diff", "--cached", "--quiet"])
        .output()?;
    // exit code 1 means there ARE changes
    Ok(!output.status.success())
}

/// Create a commit with the given message.
pub fn commit_with_message(repo: &Path, message: &str) -> Result<()> {
    run_git(repo, &["commit", "-m", message])?;
    Ok(())
}

/// Stage a file in the given repo/worktree (test helper).
#[cfg(test)]
pub(crate) fn stage_file(repo: &Path, file: &str) -> Result<()> {
    run_git(repo, &["add", file])?;
    Ok(())
}

/// Create a commit in the given repo/worktree (test helper).
#[cfg(test)]
pub(crate) fn commit(repo: &Path, message: &str) -> Result<()> {
    run_git(repo, &["commit", "-m", message])?;
    Ok(())
}

/// Return the raw `cat-file -p` output for a given revision (test helper).
#[cfg(test)]
pub(crate) fn cat_file(repo: &Path, rev: &str) -> Result<String> {
    run_git(repo, &["cat-file", "-p", rev])
}

/// Init a bare repo (test helper for simulating a remote).
#[cfg(test)]
pub(crate) fn init_bare(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    let output = std::process::Command::new("git")
        .args(["init", "--bare", &path.to_string_lossy()])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(PmError::Git(stderr));
    }
    Ok(())
}

/// Add a remote to a repo.
pub fn add_remote(repo: &Path, name: &str, url: &str) -> Result<()> {
    run_git(repo, &["remote", "add", name, url])?;
    Ok(())
}

/// Check if a named remote exists in the repo.
pub fn has_remote(repo: &Path, name: &str) -> Result<bool> {
    let remotes = run_git(repo, &["remote"])?;
    Ok(remotes.lines().any(|l| l.trim() == name))
}

/// Push a branch to a remote.
pub fn push(repo: &Path, remote: &str, branch: &str) -> Result<()> {
    run_git(repo, &["push", "-u", remote, branch])?;
    Ok(())
}

/// Get the current branch name of a repo/worktree.
pub fn current_branch(repo: &Path) -> Result<String> {
    run_git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Create a new branch from a specific start point.
pub fn create_branch_from(repo: &Path, name: &str, start_point: &str) -> Result<()> {
    run_git(repo, &["branch", name, start_point])?;
    Ok(())
}

/// Fetch a PR by number from origin into a local branch.
/// Uses GitHub's `pull/<number>/head` ref, which works for both same-repo and fork PRs.
/// Creates or force-updates the local branch to match the PR head.
pub fn fetch_pr(repo: &Path, pr_number: &str, local_branch: &str) -> Result<()> {
    run_git(
        repo,
        &[
            "fetch",
            "origin",
            &format!("pull/{pr_number}/head:{local_branch}"),
        ],
    )?;
    Ok(())
}

/// Push a branch to the remote (origin).
pub fn push_branch(repo: &Path, branch: &str) -> Result<()> {
    run_git(repo, &["push", "-u", "origin", branch])?;
    Ok(())
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

/// List remotes with their URLs.
pub fn list_remotes(repo: &Path) -> Result<String> {
    run_git(repo, &["remote", "-v"])
}

/// Get the URL of a named remote (e.g. "origin").
/// Returns `None` if the remote doesn't exist.
pub fn remote_url(repo: &Path, name: &str) -> Result<Option<String>> {
    if !has_remote(repo, name)? {
        return Ok(None);
    }
    let url = run_git(repo, &["remote", "get-url", name])?;
    let url = url.trim();
    if url.is_empty() {
        Ok(None)
    } else {
        Ok(Some(url.to_string()))
    }
}

/// Get short status output (`git status --short`).
pub fn status_short(repo: &Path) -> Result<String> {
    run_git(repo, &["status", "--short"])
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
    fn clone_repo_creates_clone() {
        let dir = tempdir().unwrap();

        // Create a bare repo as remote
        let bare_path = dir.path().join("remote.git");
        init_bare(&bare_path).unwrap();

        // Push content to it
        let staging = dir.path().join("staging");
        init_repo(&staging).unwrap();
        add_remote(&staging, "origin", &bare_path.to_string_lossy()).unwrap();
        push(&staging, "origin", "main").unwrap();

        // Clone it
        let clone_path = dir.path().join("cloned");
        clone_repo(&bare_path.to_string_lossy(), &clone_path).unwrap();

        assert!(clone_path.join(".git").exists());
        // Should have the commit from staging
        let log = run_git(&clone_path, &["log", "--oneline"]).unwrap();
        assert!(!log.is_empty());
    }

    #[test]
    fn clone_repo_fails_for_invalid_url() {
        let dir = tempdir().unwrap();
        let clone_path = dir.path().join("cloned");

        let result = clone_repo("/nonexistent/repo.git", &clone_path);
        assert!(result.is_err());
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
    fn find_worktree_for_branch_returns_path() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("main");
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
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        let found = find_worktree_for_branch(&repo_path, "feature").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn prune_worktrees_cleans_stale_entry() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("main");
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
    fn has_unpushed_commits_true_with_upstream() {
        let dir = tempdir().unwrap();
        // Create a "remote" bare repo
        let bare_path = dir.path().join("remote.git");
        std::fs::create_dir_all(&bare_path).unwrap();
        Command::new("git")
            .args(["init", "--bare", &bare_path.to_string_lossy()])
            .output()
            .unwrap();

        // Clone it to get a repo with an upstream tracking branch
        let clone_path = dir.path().join("clone");
        Command::new("git")
            .args([
                "clone",
                &bare_path.to_string_lossy(),
                &clone_path.to_string_lossy(),
            ])
            .output()
            .unwrap();

        // Create an initial commit and push so upstream exists
        run_git(&clone_path, &["commit", "--allow-empty", "-m", "initial"]).unwrap();
        run_git(&clone_path, &["push", "-u", "origin", "main"]).unwrap();

        // Add another commit locally without pushing
        run_git(&clone_path, &["commit", "--allow-empty", "-m", "unpushed"]).unwrap();

        assert!(has_unpushed_commits(&clone_path).unwrap());
    }

    #[test]
    fn has_unpushed_commits_false_when_pushed() {
        let dir = tempdir().unwrap();
        let bare_path = dir.path().join("remote.git");
        std::fs::create_dir_all(&bare_path).unwrap();
        Command::new("git")
            .args(["init", "--bare", &bare_path.to_string_lossy()])
            .output()
            .unwrap();

        let clone_path = dir.path().join("clone");
        Command::new("git")
            .args([
                "clone",
                &bare_path.to_string_lossy(),
                &clone_path.to_string_lossy(),
            ])
            .output()
            .unwrap();

        run_git(&clone_path, &["commit", "--allow-empty", "-m", "initial"]).unwrap();
        run_git(&clone_path, &["push", "-u", "origin", "main"]).unwrap();

        // Everything is pushed — should return false
        assert!(!has_unpushed_commits(&clone_path).unwrap());
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
    fn merge_no_ff_merges_branch() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();
        std::fs::write(wt_path.join("new.txt"), "content").unwrap();
        run_git(&wt_path, &["add", "new.txt"]).unwrap();
        run_git(&wt_path, &["commit", "-m", "feature commit"]).unwrap();

        merge_no_ff(&repo_path, "feature").unwrap();

        // The file from the feature branch should now be in the main worktree
        assert!(repo_path.join("new.txt").exists());
    }

    #[test]
    fn merge_no_ff_creates_merge_commit() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();
        std::fs::write(wt_path.join("new.txt"), "content").unwrap();
        run_git(&wt_path, &["add", "new.txt"]).unwrap();
        run_git(&wt_path, &["commit", "-m", "feature commit"]).unwrap();

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
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        // Create a file on main and commit
        std::fs::write(repo_path.join("shared.txt"), "main content").unwrap();
        run_git(&repo_path, &["add", "shared.txt"]).unwrap();
        run_git(&repo_path, &["commit", "-m", "main change"]).unwrap();

        // Create feature branch from before that commit
        run_git(&repo_path, &["branch", "feature", "HEAD~1"]).unwrap();

        // Add conflicting change on feature branch via worktree
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();
        std::fs::write(wt_path.join("shared.txt"), "feature content").unwrap();
        run_git(&wt_path, &["add", "shared.txt"]).unwrap();
        run_git(&wt_path, &["commit", "-m", "feature change"]).unwrap();

        let result = merge_no_ff(&repo_path, "feature");
        assert!(result.is_err());
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
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        assert_eq!(current_branch(&wt_path).unwrap(), "feature");
    }

    #[test]
    fn create_branch_from_branches_at_specific_commit() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        // Make a second commit on main
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        run_git(&repo_path, &["add", "file.txt"]).unwrap();
        run_git(&repo_path, &["commit", "-m", "second commit"]).unwrap();

        // Create a feature branch
        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        // Add a commit on the feature branch
        std::fs::write(wt_path.join("feat.txt"), "feat").unwrap();
        run_git(&wt_path, &["add", "feat.txt"]).unwrap();
        run_git(&wt_path, &["commit", "-m", "feature commit"]).unwrap();

        // Branch from "feature", not from "main"
        create_branch_from(&repo_path, "stacked", "feature").unwrap();
        let stacked_wt = dir.path().join("stacked");
        add_worktree(&repo_path, &stacked_wt, "stacked").unwrap();

        // Stacked branch should have the feature file
        assert!(stacked_wt.join("feat.txt").exists());
    }

    #[test]
    fn branch_divergence_up_to_date() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("main");
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
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        std::fs::write(wt_path.join("feat.txt"), "content").unwrap();
        run_git(&wt_path, &["add", "feat.txt"]).unwrap();
        run_git(&wt_path, &["commit", "-m", "feature commit"]).unwrap();

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
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();

        // Add commit on main
        std::fs::write(repo_path.join("main.txt"), "content").unwrap();
        run_git(&repo_path, &["add", "main.txt"]).unwrap();
        run_git(&repo_path, &["commit", "-m", "main commit"]).unwrap();

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
        let repo_path = dir.path().join("main");
        init_repo(&repo_path).unwrap();

        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        // Commit on feature
        std::fs::write(wt_path.join("feat.txt"), "content").unwrap();
        run_git(&wt_path, &["add", "feat.txt"]).unwrap();
        run_git(&wt_path, &["commit", "-m", "feature commit"]).unwrap();

        // Commit on main
        std::fs::write(repo_path.join("main.txt"), "content").unwrap();
        run_git(&repo_path, &["add", "main.txt"]).unwrap();
        run_git(&repo_path, &["commit", "-m", "main commit"]).unwrap();

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
