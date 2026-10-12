use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::Result;

use super::run_git;

/// Why pm's worktrees are locked, as `git worktree list` shows it.
const LOCK_REASON: &str = "managed by pm; remove it with `pm feat delete`";

/// Add a git worktree at the given path for the given branch, locked so that
/// `git worktree prune` run from anywhere leaves its record alone while its
/// directory is unreachable. Every removal here unlocks it first.
pub fn add_worktree(repo: &Path, worktree_path: &Path, branch: &str) -> Result<()> {
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "--lock",
            "--reason",
            LOCK_REASON,
            &worktree_path.to_string_lossy(),
            branch,
        ],
    )?;
    Ok(())
}

/// Unlock a worktree, so git may remove or prune it, returning whether it
/// was locked. Best-effort: a worktree that is unlocked or unknown to git
/// needs nothing, and any other failure surfaces from the removal or prune
/// that follows.
pub fn unlock_worktree(repo: &Path, worktree_path: &Path) -> bool {
    run_git(
        repo,
        &["worktree", "unlock", &worktree_path.to_string_lossy()],
    )
    .is_ok()
}

/// Remove a git worktree.
pub fn remove_worktree(repo: &Path, worktree_path: &Path) -> Result<()> {
    remove_unlocked(repo, worktree_path, &[])
}

/// Force-remove a git worktree (bypasses dirty check).
pub fn remove_worktree_force(repo: &Path, worktree_path: &Path) -> Result<()> {
    remove_unlocked(repo, worktree_path, &["--force"])
}

/// Remove a worktree, unlocking it first and locking it again if the
/// removal fails and leaves git's record of it.
fn remove_unlocked(repo: &Path, worktree_path: &Path, flags: &[&str]) -> Result<()> {
    let path = worktree_path.to_string_lossy();
    let was_locked = unlock_worktree(repo, worktree_path);
    let mut args = vec!["worktree", "remove"];
    args.extend_from_slice(flags);
    args.push(&path);
    if let Err(e) = run_git(repo, &args) {
        if was_locked {
            let _ = run_git(repo, &["worktree", "lock", "--reason", LOCK_REASON, &path]);
        }
        return Err(e);
    }
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

/// Whether `path` is one of `repo`'s worktrees and its directory exists. A
/// removal that fails partway drops git's record of the worktree but can
/// leave its directory; a locked record outlives a directory deleted by hand.
pub fn is_worktree(repo: &Path, path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let path = canonical(path);
    Ok(list_worktrees(repo)?
        .iter()
        .any(|w| canonical(Path::new(w)) == path))
}

/// Prune stale worktree entries (e.g. after a worktree directory is moved/deleted).
pub fn prune_worktrees(repo: &Path) -> Result<()> {
    run_git(repo, &["worktree", "prune"])?;
    Ok(())
}

/// The untracked files of `repo` that both its ignore rules exclude and a
/// pattern in `patterns` (a gitignore-format file) matches, relative to
/// `repo`'s root.
pub fn ignored_files_matching(repo: &Path, patterns: &Path) -> Result<Vec<PathBuf>> {
    let exclude_from = format!("--exclude-from={}", patterns.to_string_lossy());
    let candidates = run_git(
        repo,
        &["ls-files", "-z", "--others", "--ignored", &exclude_from],
    )?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = Command::new("git")
        .args(["-C", &repo.to_string_lossy()])
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Written from another thread: git answers as it reads, and a full
    // stdout pipe would otherwise block both sides.
    let writer = child
        .stdin
        .take()
        .map(|mut stdin| std::thread::spawn(move || stdin.write_all(candidates.as_bytes())));
    let output = child.wait_with_output()?;
    if let Some(writer) = writer {
        writer
            .join()
            .map_err(|_| std::io::Error::other("writing to git check-ignore panicked"))??;
    }
    // Exit 1 means none of the paths is ignored.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(super::failure(&output));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect())
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
    fn a_worktree_whose_removal_failed_partway_is_no_worktree() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();
        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();
        assert!(is_worktree(&repo_path, &wt_path).unwrap());
        assert!(!is_worktree(&repo_path, dir.path()).unwrap());

        // What git leaves when it cannot delete a file inside.
        unlock_worktree(&repo_path, &wt_path);
        std::fs::remove_file(wt_path.join(".git")).unwrap();
        prune_worktrees(&repo_path).unwrap();

        assert!(wt_path.exists());
        assert!(!is_worktree(&repo_path, &wt_path).unwrap());
    }

    #[test]
    fn an_added_worktree_survives_a_prune_while_its_directory_is_gone() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();
        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();

        // An unmounted volume, or a directory moved aside, looks like this.
        let aside = dir.path().join("aside");
        std::fs::rename(&wt_path, &aside).unwrap();
        prune_worktrees(&repo_path).unwrap();
        assert!(
            find_worktree_for_branch(&repo_path, "feature")
                .unwrap()
                .is_some()
        );

        unlock_worktree(&repo_path, &wt_path);
        prune_worktrees(&repo_path).unwrap();
        assert!(
            find_worktree_for_branch(&repo_path, "feature")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn ignored_files_matching_takes_only_ignored_files_the_patterns_name() {
        let dir = tempdir().unwrap();
        let repo = dir.path();
        init_repo(repo).unwrap();
        let write = |rel: &str| {
            let path = repo.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, rel).unwrap();
        };
        std::fs::write(repo.join(".gitignore"), ".env\ntarget/\nscratch\n").unwrap();
        for rel in [
            ".env",
            "sub/.env",
            "target/debug/bin",
            "scratch",
            "notes.txt",
        ] {
            write(rel);
        }
        let patterns = repo.join(".worktreeinclude");
        std::fs::write(&patterns, "**/.env\ntarget/\nnotes.txt\n").unwrap();

        let mut found = ignored_files_matching(repo, &patterns).unwrap();
        found.sort();

        let expected: Vec<PathBuf> = [".env", "sub/.env", "target/debug/bin"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn a_failed_removal_leaves_the_worktree_locked() {
        let dir = tempdir().unwrap();
        let repo_path = paths::main_worktree(dir.path());
        init_repo(&repo_path).unwrap();
        create_branch(&repo_path, "feature").unwrap();
        let wt_path = dir.path().join("feature");
        add_worktree(&repo_path, &wt_path, "feature").unwrap();
        std::fs::write(wt_path.join("dirty.txt"), "work").unwrap();

        assert!(remove_worktree(&repo_path, &wt_path).is_err());

        let listing = run_git(&repo_path, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(
            listing.contains(&format!("locked {LOCK_REASON}")),
            "{listing}"
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
}
