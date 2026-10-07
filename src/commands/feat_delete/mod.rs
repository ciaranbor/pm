//! `pm feat delete`: removing a feature once its work is safe to lose, and
//! the cleanup that delete, merge, and creation rollbacks share.

mod base;
mod cleanup;
mod safety;
mod timing;

pub use base::{MissingBase, base_scope};
pub(crate) use cleanup::cleanup_feature_with_timing;
pub use cleanup::{CleanupParams, Ended, Ending, cleanup_feature};
pub use safety::{SafetyReport, check_safety};
pub use timing::TimingLog;

use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::feature::{BaseCheckout, FeatureState, FeatureStatus, base_checkout};
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};
use crate::{gh, git, hooks};
use safety::evaluate_safety;

/// Delete a feature: kill session, remove worktree, delete branch, remove state.
pub fn feat_delete(
    project_root: &Path,
    projects_dir: &Path,
    name: &str,
    force: bool,
    tmux_server: Option<&str>,
) -> Result<Ended> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    // Load feature state
    let state = FeatureState::load(&features_dir, name)?;
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;
    let main_branch = ProjectEntry::load(projects_dir, project_name)?.main_branch;

    let worktree_path = project_root.join(&state.worktree);
    let base = state.base_branch(&main_branch);
    // Every git operation here resolves refs from any checkout of the repo,
    // so the base's own checkout is a preference, not a need; only the
    // merged-into-base safety check is unanswerable once the base is gone.
    let checkout = match base_checkout(project_root, &main_branch, base) {
        Err(PmError::BaseNotCheckedOut(_)) => BaseCheckout::main(project_root),
        other => other?,
    };
    if !force && MissingBase::probe(&checkout.worktree, base)? == MissingBase::Gone {
        return Err(MissingBase::Gone.refusal(
            &format!("cannot check whether feature '{name}' is merged"),
            name,
            base,
            &main_branch,
        ));
    }
    let base_repo = &checkout.worktree;

    // Check if the linked PR was merged on GitHub (handles squash merges
    // where git can't detect the merge). Used for both safety bypass and hook.
    let pr_merged = !state.pr.is_empty() && gh::pr_is_merged(base_repo, &state.pr).unwrap_or(false);

    // Run safety checks unless --force
    let mut warnings = Vec::new();
    let git_merged;
    let has_untracked = if !force {
        let live = git::is_worktree(base_repo, &worktree_path)?;
        let report = check_safety(
            live.then_some(worktree_path.as_path()),
            base_repo,
            &state.branch,
            base,
        )?;
        evaluate_safety(&report, pr_merged, name)?;
        git_merged = report.is_merged;

        if report.has_warnings() {
            warnings.push(format!(
                "feature '{name}' had {} untracked file(s), deleted with it: {}",
                report.untracked_files.len(),
                report.untracked_files.join(", ")
            ));
        }
        !report.untracked_files.is_empty()
    } else {
        git_merged = git::branch_merged_into(base_repo, &state.branch, base).unwrap_or(false);
        false
    };

    let ending = if pr_merged || state.status == FeatureStatus::Merged {
        Ending::Merged
    } else if !git_merged {
        Ending::Deleted
    } else if git::branch_commits_pointed_at(base_repo, &state.branch)? == 1 {
        Ending::DeletedEmpty
    } else {
        Ending::Merged
    };

    // Force-remove worktree if --force was passed or if there are untracked files
    // (git worktree remove refuses untracked files without --force, but we've
    // already warned the user about them in the safety checks above)
    let force_worktree = force || has_untracked;

    let own = Ended::own_session(tmux_server, project_name, name, &checkout.scope);
    warnings.extend(cleanup_feature(&CleanupParams {
        repo: base_repo,
        worktree_path: &worktree_path,
        branch: &state.branch,
        features_dir: &features_dir,
        name,
        project_name,
        force_worktree,
        worktree_created: true,
        tmux_server,
        kill_session: own.is_none(),
        delete_branch: true,
        best_effort: false,
        base_scope: &checkout.scope,
        ending: Some(ending),
    })?);

    // Trigger post-merge hook when deleting a feature whose PR was merged
    if pr_merged {
        let hook_path = project_root.join(hooks::POST_MERGE_PATH);
        hooks::run_hook(
            tmux_server,
            &hooks::HookContext::post_merge(project_root, project_name, &checkout.scope, name),
            &hook_path,
        );
    }

    Ok(Ended { warnings, own })
}

#[cfg(test)]
mod tests;
