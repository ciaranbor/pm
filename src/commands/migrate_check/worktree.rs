//! A branch and its worktree: commits not on origin, uncommitted changes,
//! untracked files pm would not write again, a rebase left half done, and
//! harness settings files git does not track.

use std::path::{Path, PathBuf};

use crate::commands::skills;
use crate::error::Result;
use crate::git;
use crate::harness::Harness;

use super::line::{Line, count};
use super::repo::{self, Remote, Sync};
use super::{Finding, Step, rel_path};

/// A harness's per-worktree settings files (permissions and the like) that
/// git does not track, so a clone lacks them: main's, and a feature's that
/// differs from main's (a restored feature is seeded from main).
pub(super) fn settings_findings(
    root: &Path,
    project: &str,
    harness: Harness,
    worktrees: &[PathBuf],
) -> Result<Vec<Finding>> {
    let mut out = Vec::new();
    let Some((main, _)) = worktrees.split_first() else {
        return Ok(out);
    };
    for file in harness.seeded_files() {
        let rel = Path::new(harness.config_dir()).join(file);
        let main_text = std::fs::read_to_string(main.join(&rel)).ok();
        for worktree in worktrees {
            let Ok(text) = std::fs::read_to_string(worktree.join(&rel)) else {
                continue;
            };
            let trivial = text.trim().is_empty() || text.trim() == "{}";
            let inherited = worktree != main && main_text.as_deref() == Some(text.as_str());
            if trivial || inherited || !git::ls_files(worktree, &rel.to_string_lossy())?.is_empty()
            {
                continue;
            }
            out.push(Finding::manual(
                project,
                format!(
                    "copy {} (git doesn't track it)",
                    rel_path(root, &worktree.join(&rel))
                ),
                Some(format!(
                    "{harness}'s settings for that worktree: the new host's clone won't have them"
                )),
            ));
        }
    }
    Ok(out)
}

/// Untracked paths in a worktree that pm writes again on the new host
/// (`pm restore` projects main's assets and seeds each feature): every
/// harness's projections and seeded settings, and for a feature its seeded
/// copy of main's canonical skills. Settings that differ from what restore
/// would write are [`settings_findings`]'.
fn regenerated(feature: bool) -> Vec<String> {
    let mut out = Vec::new();
    for harness in Harness::SUPPORTED {
        let dir = harness.config_dir();
        out.extend(
            harness
                .projected_dirs()
                .iter()
                .map(|d| format!("{dir}/{d}/")),
        );
        out.extend(harness.seeded_files().iter().map(|f| format!("{dir}/{f}")));
    }
    if feature {
        out.push(format!("{}/skills/", skills::CANONICAL_DIR));
    }
    out
}

/// A code branch to check: where its repo is, and its worktree if one is
/// checked out here.
pub(super) struct Branch<'a> {
    pub root: &'a Path,
    /// The main worktree, which holds the repo.
    pub main: &'a Path,
    pub name: &'a str,
    pub worktree: Option<&'a Path>,
    /// A feature's worktree, seeded with main's canonical skills.
    pub feature: bool,
    /// A feature's base, which `pm restore` creates the branch from when
    /// origin lacks it.
    pub base: Option<&'a str>,
    /// The branch on origin that creation starts from: `base`, or, for a
    /// base `pm restore` creates too, that one's start.
    pub start: Option<&'a str>,
}

/// What a branch's line found, beside the line itself.
pub(super) struct Checked {
    pub line: Line,
    /// Uncommitted or untracked work in the worktree.
    pub dirty: bool,
    /// origin is ahead and the branch has nothing more.
    pub behind: bool,
    /// Not on origin, with no commits of its own: the base it is created
    /// from on the new host.
    pub from_base: Option<String>,
}

/// The problems of `branch` and its worktree, and the git commands, run
/// from the root, that put its work on `remote`.
pub(super) fn check(branch: &Branch<'_>, remote: Option<&Remote>) -> Result<Checked> {
    let dir = rel_path(branch.root, branch.worktree.unwrap_or(branch.main));
    let mut line = Line::in_dir(&dir);
    let push = format!("push -u origin {}", branch.name);
    let mut dirty = false;
    if let Some(worktree) = branch.worktree {
        // Committing or pushing mid-rebase fails or loses the rebase: the
        // rebase is the whole fix, and the next check gives the push.
        if git::rebase_in_progress(worktree).unwrap_or(false) {
            line.problem("rebase in progress");
            line.git("rebase --continue");
            line.detail("resolve and `git add` any conflicts first, or `git rebase --abort`");
            line.step(Step::Repair);
            return Ok(Checked {
                line,
                dirty: true,
                behind: false,
                from_base: None,
            });
        }
        let (tracked, untracked) = repo::changes(worktree, &regenerated(branch.feature))?;
        dirty = !tracked.is_empty() || !untracked.is_empty();
        if !tracked.is_empty() {
            line.problem(format!("{} modified", tracked.len()));
            line.files("modified", &tracked);
        }
        if !untracked.is_empty() {
            line.problem(format!("{} untracked", untracked.len()));
            line.files("untracked", &untracked);
            line.detail("untracked files may be secrets or scratch: add what belongs in the repo, delete or .gitignore the rest");
            line.git("add <file>…");
        }
        if dirty {
            line.git(if tracked.is_empty() {
                "commit"
            } else {
                "commit -a"
            });
        }
    }
    let Some(remote) = remote else {
        if line.has_git() {
            line.step(Step::Branches);
        }
        return Ok(Checked {
            line,
            dirty,
            behind: false,
            from_base: None,
        });
    };
    let mut behind = false;
    let mut from_base = None;
    let mut repair = false;
    match repo::upstream(branch.main, remote, branch.name)? {
        Sync::NoLocal => {}
        Sync::Pushed => {
            if dirty {
                line.git(&push);
            }
        }
        Sync::Behind => {
            behind = true;
            if dirty {
                line.git(format!("pull --rebase origin {}", branch.name));
                line.git(&push);
            }
        }
        Sync::NotOnRemote => match (branch.base, branch.start) {
            (Some(base), Some(start))
                if !dirty && remote.base_holds(branch.main, branch.name, start)? =>
            {
                from_base = Some(base.to_string());
            }
            _ => {
                line.problem("not on origin");
                line.git(&push);
            }
        },
        Sync::Ahead(n) => {
            line.problem(count(n, "commit") + " not pushed");
            line.git(&push);
        }
        Sync::Diverged => {
            line.problem("diverged from origin");
            match branch.worktree {
                Some(_) => line.git(format!("pull --rebase origin {}", branch.name)),
                None => line.command(repo::rebase_onto_origin(
                    branch.root,
                    branch.main,
                    branch.name,
                )?),
            }
            line.git(&push);
        }
        Sync::Unfetched => {
            line.problem("origin has commits not fetched here");
            line.command(format!(
                "git -C {} fetch origin",
                rel_path(branch.root, branch.main)
            ));
            line.detail("fetch, then run this check again");
            line.step(Step::Repair);
            repair = true;
        }
        Sync::Unreachable(why) => {
            line.problem("origin unreachable");
            line.detail(why);
            line.command(format!(
                "git -C {} ls-remote origin",
                rel_path(branch.root, branch.main)
            ));
            line.step(Step::Repair);
            repair = true;
        }
    }
    if dirty || (!repair && !line.is_empty()) {
        line.step(Step::Branches);
    }
    Ok(Checked {
        line,
        dirty,
        behind,
        from_base,
    })
}
