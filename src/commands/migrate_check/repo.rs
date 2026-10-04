//! Whether a repo's work has reached its `origin`, judged against what the
//! remote holds now (`git ls-remote`), not the last fetch.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::Result;
use crate::git;

use super::{Finding, shell_path};

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
enum Sync {
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

/// The command that brings origin's commits into `branch` of `repo`, run
/// where the branch is checked out: in its worktree, or in one added for
/// it beside the repo.
fn rebase_onto_origin(repo: &Path, branch: &str) -> Result<String> {
    let pull = |at: &Path| format!("git -C {} pull --rebase origin {branch}", shell_path(at));
    Ok(match git::find_worktree_for_branch(repo, branch)? {
        Some(worktree) => pull(&worktree),
        None => {
            let at = repo.parent().unwrap_or(repo).join(branch.replace('/', "-"));
            format!(
                "git -C {} worktree add {} {branch} && {}",
                shell_path(repo),
                shell_path(&at),
                pull(&at)
            )
        }
    })
}

/// The finding for `branch` of `repo` against `remote`, `label` naming it
/// in the report and `push` the command that publishes it; `None` when
/// nothing would be lost. `reconcile` merges origin's commits in when both
/// sides moved; `None` rebases the branch where it is checked out.
pub(super) fn branch_finding(
    repo: &Path,
    remote: &Remote,
    branch: &str,
    label: &str,
    push: &str,
    reconcile: Option<&str>,
) -> Result<Option<Finding>> {
    let heads = match &remote.heads {
        Ok(heads) => heads,
        Err(why) => {
            return Ok(Some(Finding::blocker(
                format!("{label}: origin could not be reached ({why})"),
                format!(
                    "check access with `git -C {} ls-remote origin`",
                    shell_path(repo)
                ),
            )));
        }
    };
    let repo_arg = shell_path(repo);
    Ok(match sync(repo, branch, heads)? {
        Sync::Pushed | Sync::NoLocal => None,
        Sync::Behind => Some(Finding::note(format!(
            "{label}: origin is ahead of the local branch; the new host gets origin's"
        ))),
        Sync::NotOnRemote => Some(Finding::blocker(
            format!("{label}: branch {branch} is not on origin"),
            push.to_string(),
        )),
        Sync::Ahead(n) => Some(Finding::blocker(
            format!(
                "{label}: {n} commit{} not pushed",
                if n == 1 { "" } else { "s" }
            ),
            push.to_string(),
        )),
        Sync::Diverged => {
            let reconcile = match reconcile {
                Some(command) => command.to_string(),
                None => rebase_onto_origin(repo, branch)?,
            };
            Some(Finding::blocker(
                format!("{label}: branch {branch} and origin have diverged"),
                format!("{reconcile}, then {push}"),
            ))
        }
        Sync::Unfetched => Some(Finding::blocker(
            format!("{label}: origin has commits this clone has not fetched"),
            format!("git -C {repo_arg} fetch origin, then run this check again"),
        )),
    })
}

/// Up to a few of `paths`, and how many more there are.
fn sample(paths: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut listed: Vec<String> = paths.iter().take(SHOWN).cloned().collect();
    if paths.len() > SHOWN {
        listed.push(format!("{} more", paths.len() - SHOWN));
    }
    listed.join(", ")
}

/// Findings for work in `worktree` that is not committed: changes to
/// tracked files, fixed by committing them and running `push`, and
/// untracked files, which may be secrets or scratch, so the fix never adds
/// them wholesale. Untracked files under a `regenerated` prefix are pm's to
/// write again on the new host, so they are not reported.
pub(super) fn dirty_findings(
    worktree: &Path,
    label: &str,
    push: &str,
    regenerated: &[String],
) -> Result<Vec<Finding>> {
    let changed = git::changed_paths(worktree)?;
    let (untracked, tracked): (Vec<&String>, Vec<&String>) =
        changed.iter().partition(|l| l.starts_with("??"));
    let untracked: Vec<&String> = untracked
        .into_iter()
        .filter(|l| {
            let path = l.get(3..).unwrap_or(l);
            !regenerated
                .iter()
                .any(|prefix| path.starts_with(prefix.as_str()))
        })
        .collect();
    let paths = |lines: Vec<&String>| -> Vec<String> {
        lines
            .iter()
            .map(|l| l.get(3..).unwrap_or(l).to_string())
            .collect()
    };
    let arg = shell_path(worktree);
    let mut out = Vec::new();
    if !tracked.is_empty() {
        out.push(Finding::blocker(
            format!("{label}: uncommitted changes ({})", sample(&paths(tracked))),
            format!("git -C {arg} commit -a && {push}"),
        ));
    }
    if !untracked.is_empty() {
        out.push(Finding::blocker(
            format!(
                "{label}: untracked files don't travel ({})",
                sample(&paths(untracked))
            ),
            format!(
                "git -C {arg} add <file>… && git -C {arg} commit && {push} for what belongs in \
                 the repo; delete or .gitignore the rest"
            ),
        ));
    }
    Ok(out)
}

/// One finding for any uncommitted or untracked file in a state repo,
/// whose push commits everything (`git add -A`).
pub(super) fn state_dirty_finding(dir: &Path, label: &str, push: &str) -> Result<Option<Finding>> {
    let changed = git::changed_paths(dir)?;
    if changed.is_empty() {
        return Ok(None);
    }
    let paths: Vec<String> = changed.iter().map(|l| l.trim().to_string()).collect();
    Ok(Some(Finding::blocker(
        format!("{label}: changes not committed ({})", sample(&paths)),
        push.to_string(),
    )))
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
