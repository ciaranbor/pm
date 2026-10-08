//! `pm feat merge`: merging a feature branch into its base from the base's
//! checkout, then ending the feature unless `--keep`.
//!
//! What must hold before a merge starts is the `check` module's. A branch
//! already merged locally, or upstream once its base's tracking branch is
//! fetched, is not merged again; the base is pulled instead. A failed merge
//! is aborted so the base worktree is left clean. The feature is recorded
//! `Merged` before cleanup, so a retry after a cleanup that failed partway
//! only cleans up.

use std::path::Path;
use std::time::Instant;

use crate::commands::feat_delete::{
    CleanupParams, Ended, Ending, TimingLog, cleanup_feature_with_timing,
};
use crate::error::{PmError, Result};
use crate::git;
use crate::hooks;
use crate::state::feature::{FeatureStatus, Progress};
use crate::state::paths;

mod check;
use check::Plan;

/// Merge a feature branch into its base branch from the base's checkout.
/// By default, cleans up the feature afterwards (remove worktree, delete branch, remove state, kill session).
/// With `keep`, preserve the feature instead.
pub fn feat_merge(
    project_root: &Path,
    projects_dir: &Path,
    name: &str,
    keep: bool,
    tmux_server: Option<&str>,
) -> Result<Ended> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);
    let plan = Plan::load(project_root, projects_dir, name)?;
    let checkout = plan.check(keep)?.map_err(|refusal| plan.refuse(refusal))?;
    let Plan {
        state,
        project_name,
        worktree: worktree_path,
        ..
    } = &plan;
    let project_name = project_name.as_str();
    let base = plan.base();
    let base_repo = &checkout.worktree;

    let merge_start = Instant::now();
    let mut tlog: Option<TimingLog> = Some(TimingLog::new(&pm_dir, "merge", name));
    let already_status_merged = plan.already_merged();
    if state.progress != Progress::Ready {
        eprintln!(
            "warning: feature '{name}' is not marked ready (status: {})",
            state.progress
        );
    }

    if already_status_merged {
        eprintln!("Feature '{name}' already merged — cleaning up");
    } else {
        // Check if the branch is already merged locally
        let check_start = Instant::now();
        let mut already_merged = git::branch_merged_into(base_repo, &state.branch, base)?;

        // If not merged locally, check whether the base has an upstream and, if so,
        // fetch and re-check against it. tracking_branch is a local lookup, so
        // checking it first lets us skip the network fetch entirely when there is
        // no upstream. The branch may have been merged upstream (e.g. via GitHub PR).
        if !already_merged
            && let Ok(Some(tracking)) = git::tracking_branch(base_repo, base)
            && let Ok(()) = git::fetch(base_repo)
        {
            already_merged = git::branch_merged_into(base_repo, &state.branch, &tracking)?;
        }

        if let Some(tl) = tlog.as_mut() {
            tl.record("merge-check+fetch", check_start.elapsed());
        }

        if already_merged {
            // Branch was merged (locally or upstream) — pull to update local base
            let pull_start = Instant::now();
            if let Err(e) = git::pull(base_repo) {
                eprintln!("warning: git pull failed: {e}");
            }
            if let Some(tl) = tlog.as_mut() {
                tl.record("git-pull", pull_start.elapsed());
            }
        } else {
            // Perform the merge from the base worktree
            let merge_ff_start = Instant::now();
            if let Err(e) = git::merge_no_ff(base_repo, &state.branch) {
                // Abort the failed merge to leave base worktree clean
                if let Err(abort_err) = git::merge_abort(base_repo) {
                    eprintln!("Warning: merge --abort failed: {abort_err}");
                    return Err(e);
                }
                let why = match &e {
                    PmError::Git(stderr) if !stderr.is_empty() => format!(": {stderr}"),
                    _ => String::new(),
                };
                return Err(PmError::MergeAborted(format!(
                    "git could not merge feature '{name}' into '{base}'{why}; \
                     the merge was aborted, so nothing changed"
                )));
            }
            if let Some(tl) = tlog.as_mut() {
                tl.record("merge-no-ff", merge_ff_start.elapsed());
            }
        }

        // Run post-merge hook in a named "hook" window within the base session
        let hook_start = Instant::now();
        let hook_path = project_root.join(hooks::POST_MERGE_PATH);
        hooks::run_hook(
            tmux_server,
            &hooks::HookContext::post_merge(project_root, project_name, &checkout.scope, name),
            &hook_path,
        );
        if let Some(tl) = tlog.as_mut() {
            tl.record("post-merge-hook", hook_start.elapsed());
        }
    }

    // Recorded before any cleanup, so a retry after one that failed partway
    // only cleans up.
    let mut updated = state.clone();
    updated.status = FeatureStatus::Merged;
    updated.save(&features_dir, name)?;

    if keep {
        // Flush timing log for --keep (no cleanup phase follows)
        if let Some(tl) = tlog.as_mut() {
            tl.record_total(merge_start.elapsed());
            tl.flush();
        }
        Ok(Ended::default())
    } else {
        let own = Ended::own_session(tmux_server, project_name, name, &checkout.scope);
        let warnings = cleanup_feature_with_timing(
            &CleanupParams {
                repo: base_repo,
                worktree_path,
                branch: &state.branch,
                features_dir: &features_dir,
                name,
                project_name,
                force_worktree: true, // always force — both paths checked for uncommitted changes above
                worktree_created: true,
                tmux_server,
                kill_session: own.is_none(),
                delete_branch: true,
                best_effort: false,
                base_scope: &checkout.scope,
                ending: Some(Ending::Merged),
            },
            &mut tlog,
        )?;
        Ok(Ended { warnings, own })
    }
}

/// Why merging feature `name` from a device would not land the branch as
/// it stands, in a few words; `None` when it would. Beyond [`feat_merge`]'s
/// own refusals, the branch must contain its base: a merge of one behind it
/// would bring in base commits the feature was never checked against.
pub fn blocker(project_root: &Path, projects_dir: &Path, name: &str) -> Result<Option<String>> {
    let plan = Plan::load(project_root, projects_dir, name)?;
    let checkout = match plan.check(false)? {
        Ok(checkout) => checkout,
        Err(refusal) => return Ok(Some(plan.brief(refusal))),
    };
    if plan.already_merged() {
        return Ok(None);
    }
    let (repo, branch, base) = (&checkout.worktree, &plan.state.branch, plan.base());
    let on_top = git::branch_merged_into(repo, base, branch)?
        || git::branch_merged_into(repo, branch, base)?;
    Ok((!on_top).then(|| format!("Behind {base}: rebase first")))
}

#[cfg(test)]
mod tests;
