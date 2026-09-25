use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};

use super::run_git;

/// Initialize a new git repository at the given path with an initial commit
/// on `main`, whatever `init.defaultBranch` says.
pub fn init_repo(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;

    let output = Command::new("git")
        .args(["init", &path.to_string_lossy()])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(PmError::Git(stderr));
    }

    // Retargeting the unborn HEAD works on every git version, unlike `init -b`.
    run_git(path, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
    run_git(path, &["commit", "--allow-empty", "-m", "Initial commit"])?;

    Ok(())
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
    fn init_repo_commits_on_main_regardless_of_default_branch() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        std::fs::create_dir_all(&repo_path).unwrap();
        // Leave an unborn HEAD pointing at master, as `git init` does when
        // init.defaultBranch is master (or unset on git < 2.28).
        run_git(&repo_path, &["-c", "init.defaultBranch=master", "init"]).unwrap();
        assert_eq!(
            run_git(&repo_path, &["symbolic-ref", "HEAD"]).unwrap(),
            "refs/heads/master"
        );

        init_repo(&repo_path).unwrap();

        assert_eq!(
            run_git(&repo_path, &["symbolic-ref", "--short", "HEAD"]).unwrap(),
            "main"
        );
        assert!(run_git(&repo_path, &["rev-parse", "--verify", "refs/heads/main"]).is_ok());
        assert!(run_git(&repo_path, &["rev-parse", "--verify", "refs/heads/master"]).is_err());
    }

    #[test]
    fn init_repo_allows_git_status() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");

        init_repo(&repo_path).unwrap();

        let result = run_git(&repo_path, &["status"]);
        assert!(result.is_ok());
    }
}
