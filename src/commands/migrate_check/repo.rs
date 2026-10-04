//! Whether a repo's work has reached its `origin`, judged against what the
//! remote holds now (`git ls-remote`), not the last fetch.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::Result;
use crate::git;

use super::line::{Line, count};
use super::{Finding, Step, rel_path};

/// The branches `origin` holds, read once per repo; `Err` carries why the
/// remote could not be asked.
pub(super) struct Remote {
    heads: std::result::Result<BTreeMap<String, String>, String>,
}

impl Remote {
    /// `origin` of `repo`, or `None` when it has none.
    pub(super) fn of(repo: &Path) -> Result<Option<Self>> {
        if !git::has_remote(repo, "origin")? {
            return Ok(None);
        }
        Ok(Some(Self {
            heads: git::remote_heads(repo, "origin").map_err(|e| e.to_string()),
        }))
    }
}

/// How a local branch stands against `origin`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Sync {
    Pushed,
    /// `origin` has commits the branch lacks and nothing more: the new host
    /// gets origin's.
    Behind,
    NotOnRemote,
    Ahead(usize),
    Diverged,
    /// `origin` points at a commit this repo has not fetched.
    Unfetched,
    /// No such local branch: nothing here to lose.
    NoLocal,
    /// `origin` could not be asked, and why.
    Unreachable(String),
}

fn sync(repo: &Path, branch: &str, heads: &BTreeMap<String, String>) -> Result<Sync> {
    let Some(local) = git::branch_commit(repo, branch)? else {
        return Ok(Sync::NoLocal);
    };
    let Some(remote) = heads.get(branch) else {
        return Ok(Sync::NotOnRemote);
    };
    if *remote == local {
        return Ok(Sync::Pushed);
    }
    if !git::has_commit(repo, remote) {
        return Ok(Sync::Unfetched);
    }
    let divergence = git::branch_divergence(repo, &local, remote)?;
    Ok(match (divergence.ahead, divergence.behind) {
        (0, _) => Sync::Behind,
        (ahead, 0) => Sync::Ahead(ahead),
        _ => Sync::Diverged,
    })
}

/// The command, run from `root`, that brings origin's commits into
/// `branch` of `repo` where it is checked out: in its worktree, or in one
/// added for it beside the repo.
pub(super) fn rebase_onto_origin(root: &Path, repo: &Path, branch: &str) -> Result<String> {
    let pull = |at: &Path| {
        format!(
            "git -C {} pull --rebase origin {branch}",
            rel_path(root, at)
        )
    };
    Ok(match git::find_worktree_for_branch(repo, branch)? {
        Some(worktree) => pull(&worktree),
        None => {
            let at = repo.parent().unwrap_or(repo).join(branch.replace('/', "-"));
            format!(
                "git -C {} worktree add {} {branch} && {}",
                rel_path(root, repo),
                rel_path(root, &at),
                pull(&at)
            )
        }
    })
}

/// How `branch` of `repo` stands against `remote`.
pub(super) fn upstream(repo: &Path, remote: &Remote, branch: &str) -> Result<Sync> {
    match &remote.heads {
        Ok(heads) => sync(repo, branch, heads),
        Err(why) => Ok(Sync::Unreachable(why.clone())),
    }
}

/// The paths in `worktree` with changes to tracked files, and its
/// untracked files outside the `regenerated` prefixes pm writes again on
/// the new host.
pub(super) fn changes(
    worktree: &Path,
    regenerated: &[String],
) -> Result<(Vec<String>, Vec<String>)> {
    let mut tracked = Vec::new();
    let mut untracked = Vec::new();
    for line in git::changed_paths(worktree)? {
        let path = line.get(3..).unwrap_or(&line).to_string();
        if !line.starts_with("??") {
            tracked.push(path);
        } else if !regenerated.iter().any(|p| path.starts_with(p.as_str())) {
            untracked.push(path);
        }
    }
    Ok((tracked, untracked))
}

/// Adds a state repo's problems to `line`: anything uncommitted or not on
/// `remote`, all of which `push` (commit everything, then push) clears;
/// `pull` merges in origin's commits when both sides moved. `dir` is the
/// repo as written from the root, and `subject` the line's. Returns a note
/// when origin is merely ahead.
pub(super) fn state_repo(
    line: &mut Line,
    repo: &Path,
    dir: &str,
    subject: &str,
    remote: Option<&Remote>,
    push: Step,
    pull: &str,
) -> Result<Option<Finding>> {
    let changed = git::changed_paths(repo)?;
    if !changed.is_empty() {
        line.problem(count(changed.len(), "change") + " not committed");
        let paths: Vec<String> = changed
            .iter()
            .map(|l| l.get(3..).unwrap_or(l).to_string())
            .collect();
        line.files("changed", &paths);
        line.step(push);
    }
    let Some(remote) = remote else {
        return Ok(None);
    };
    let branch = git::current_branch(repo)?;
    match upstream(repo, remote, &branch)? {
        Sync::Pushed | Sync::NoLocal => {}
        Sync::Behind => {
            return Ok(Some(Finding::note(
                subject,
                "origin is ahead of the local branch; the new host gets origin's".to_string(),
            )));
        }
        Sync::NotOnRemote => {
            line.problem("not on origin");
            line.step(push);
        }
        Sync::Ahead(n) => {
            line.problem(count(n, "commit") + " not pushed");
            line.step(push);
        }
        Sync::Diverged => {
            line.problem("diverged from origin");
            line.command(pull);
            line.step(push);
        }
        Sync::Unfetched => {
            line.problem("origin has commits not fetched here");
            line.command(format!("git -C {dir} fetch origin"));
            line.detail("fetch, then run this check again");
            line.step(Step::Repair);
        }
        Sync::Unreachable(why) => {
            line.problem("origin unreachable");
            line.detail(why);
            line.command(format!("git -C {dir} ls-remote origin"));
            line.step(Step::Repair);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{init_bare, init_repo};
    use tempfile::tempdir;

    fn commit(repo: &Path, file: &str) {
        std::fs::write(repo.join(file), file).unwrap();
        git::add_all(repo).unwrap();
        git::commit_with_message(repo, file).unwrap();
    }

    /// A repo whose `main` is pushed to a bare `origin`, and a second clone
    /// of that origin.
    fn pushed(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let bare = dir.join("origin.git");
        init_bare(&bare).unwrap();
        let repo = dir.join("repo");
        init_repo(&repo).unwrap();
        git::add_remote(&repo, "origin", &bare.to_string_lossy()).unwrap();
        git::push(&repo, "origin", "main").unwrap();
        let other = dir.join("other");
        git::clone_repo(&bare.to_string_lossy(), &other).unwrap();
        (repo, other)
    }

    fn state(repo: &Path, branch: &str) -> Sync {
        let heads = git::remote_heads(repo, "origin").unwrap();
        sync(repo, branch, &heads).unwrap()
    }

    #[test]
    fn a_branch_reads_against_what_origin_holds_now() {
        let dir = tempdir().unwrap();
        let (repo, other) = pushed(dir.path());
        assert_eq!(state(&repo, "main"), Sync::Pushed);

        commit(&repo, "a");
        commit(&repo, "b");
        assert_eq!(state(&repo, "main"), Sync::Ahead(2));

        git::create_branch(&repo, "feat").unwrap();
        assert_eq!(state(&repo, "feat"), Sync::NotOnRemote);
        assert_eq!(state(&repo, "gone"), Sync::NoLocal);

        // origin moves on from another clone: this one hasn't fetched it.
        commit(&other, "c");
        git::push(&other, "origin", "main").unwrap();
        assert_eq!(state(&repo, "main"), Sync::Unfetched);
        git::fetch_remote(&repo, "origin").unwrap();
        assert_eq!(state(&repo, "main"), Sync::Diverged);
    }

    #[test]
    fn a_branch_origin_is_ahead_of_loses_nothing() {
        let dir = tempdir().unwrap();
        let (repo, other) = pushed(dir.path());
        commit(&other, "c");
        git::push(&other, "origin", "main").unwrap();
        git::fetch_remote(&repo, "origin").unwrap();
        assert_eq!(state(&repo, "main"), Sync::Behind);
    }
}
