use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};

use super::run_git;

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
    let output = std::process::Command::new("git")
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

/// Get short status output (`git status --short`).
pub fn status_short(repo: &Path) -> Result<String> {
    run_git(repo, &["status", "--short"])
}

/// Check if a path is a git repository (has .git dir or file).
pub fn is_git_repo(path: &Path) -> bool {
    let git_path = path.join(".git");
    git_path.exists()
}

/// Check if a ref exists in the repo (e.g. `origin/main`, `refs/remotes/origin/main`).
pub fn ref_exists(repo: &Path, refspec: &str) -> Result<bool> {
    let result = run_git(repo, &["rev-parse", "--verify", refspec]);
    match result {
        Ok(_) => Ok(true),
        Err(PmError::Git(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Remove a path from the git index without deleting it from disk.
/// Equivalent to `git rm --cached -r <path>`.
pub fn rm_cached(repo: &Path, path: &str) -> Result<()> {
    run_git(repo, &["rm", "--cached", "-r", path])?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::init_repo;
    use std::process::Command;
    use tempfile::tempdir;

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
        stage_file(&repo_path, "file.txt").unwrap();

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
