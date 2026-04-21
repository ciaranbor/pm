use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};

use super::run_git;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{init_bare, init_repo};
    use tempfile::tempdir;

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
}
