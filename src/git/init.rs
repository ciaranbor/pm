use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};

use super::run_git;

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
    fn init_repo_allows_git_status() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");

        init_repo(&repo_path).unwrap();

        let result = run_git(&repo_path, &["status"]);
        assert!(result.is_ok());
    }
}
