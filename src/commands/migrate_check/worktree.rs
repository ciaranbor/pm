//! A worktree's work that a clone would not carry: uncommitted changes,
//! untracked files pm would not write again, a rebase left half done, and
//! harness settings files git does not track.

use std::path::{Path, PathBuf};

use crate::commands::skills;
use crate::error::Result;
use crate::git;
use crate::harness::Harness;

use super::{Finding, repo, shell_path};

/// A harness's per-worktree settings files (permissions and the like) that
/// git does not track, so a clone lacks them: main's, and a feature's that
/// differs from main's (a restored feature is seeded from main).
pub(super) fn settings_findings(harness: Harness, worktrees: &[PathBuf]) -> Result<Vec<Finding>> {
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
            out.push(Finding::manual(format!(
                "{} is not tracked by git, so the new host won't have it: carry it by hand",
                shell_path(&worktree.join(&rel))
            )));
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

/// A worktree's uncommitted work and git operations left half done;
/// `push` publishes its branch.
pub(super) fn worktree_findings(
    worktree: &Path,
    label: &str,
    push: &str,
    feature: bool,
) -> Result<Vec<Finding>> {
    let mut out = Vec::new();
    if git::rebase_in_progress(worktree).unwrap_or(false) {
        let arg = shell_path(worktree);
        out.push(Finding::blocker(
            format!("{label}: a rebase is in progress"),
            format!("git -C {arg} rebase --continue (or --abort)"),
        ));
    }
    out.extend(repo::dirty_findings(
        worktree,
        label,
        push,
        &regenerated(feature),
    )?);
    Ok(out)
}
