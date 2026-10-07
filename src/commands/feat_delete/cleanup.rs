//! Removing a feature's resources, shared by delete, merge, and the
//! creation rollbacks, and telling `main` how the feature ended.

use std::path::Path;
use std::time::Instant;

use super::TimingLog;
use crate::error::Result;
use crate::state::agent::AgentRegistry;
use crate::state::feature::FeatureState;
use crate::{git, messages, tmux};

/// Parameters for feature cleanup.
pub struct CleanupParams<'a> {
    pub repo: &'a Path,
    pub worktree_path: &'a Path,
    pub branch: &'a str,
    pub features_dir: &'a Path,
    pub name: &'a str,
    pub project_name: &'a str,
    pub force_worktree: bool,
    /// Whether pm created the worktree at `worktree_path`, so a directory
    /// there that git no longer knows is what a failed removal left, and
    /// pm's to delete. False for a creation rollback, where it may be the
    /// user's directory that blocked the worktree.
    pub worktree_created: bool,
    pub tmux_server: Option<&'a str>,
    /// False leaves the feature's session to the caller: `pm delete` run
    /// from inside it kills it once the rest of the project is gone.
    pub kill_session: bool,
    /// Whether to delete the branch as part of cleanup. Set this to `false`
    /// when rolling back a `feat_adopt` failure, since the branch is owned by
    /// the user and must not be destroyed.
    pub delete_branch: bool,
    /// When true, each cleanup step is run independently and errors are
    /// swallowed. Used by creation-flow rollback where a failure in an
    /// earlier step (e.g. removing a blocker directory that git doesn't
    /// know about) must not prevent state/agent/message cleanup from
    /// running. Regular `feat_delete` leaves this false so errors surface
    /// to the user.
    pub best_effort: bool,
    /// The scope whose session the client is switched to when it was
    /// attached to the one being killed.
    pub base_scope: &'a str,
    /// How the feature ended, for `main`'s notice; `None` tells no one and
    /// leaves the summary alone (a rollback, or the project going too).
    pub ending: Option<Ending>,
}

/// What a finished delete or merge leaves its caller.
#[derive(Debug, Default)]
pub struct Ended {
    /// What the CLI warns of: untracked files deleted with the worktree,
    /// what of it could not be removed.
    pub warnings: Vec<String>,
    /// The feature's session, when this process runs in it.
    pub own: Option<tmux::OwnSession>,
}

impl Ended {
    /// The feature's session, as [`tmux::OwnSession::is`], its clients going
    /// to `base_scope`'s.
    pub(crate) fn own_session(
        tmux_server: Option<&str>,
        project_name: &str,
        name: &str,
        base_scope: &str,
    ) -> Option<tmux::OwnSession> {
        tmux::OwnSession::is(
            tmux_server,
            &tmux::session_name(project_name, name),
            Some(tmux::session_name(project_name, base_scope)),
        )
    }
}

/// How a feature ended. `main` triages its summary differently: a deleted
/// feature's changes never landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Merged,
    Deleted,
    /// Deleted with no commits of its own, which git alone would call
    /// merged. A branch fast-forwarded onto an advanced base has moved, so
    /// it still reads as `Merged`.
    DeletedEmpty,
}

/// `main`'s notice that `name` ended, its triage trigger.
fn ending_body(name: &str, ending: Ending, summary: Option<&Path>) -> String {
    let what = match ending {
        Ending::Merged => "was merged",
        Ending::Deleted => "was deleted without being merged: its changes never landed",
        Ending::DeletedEmpty => "was deleted with no commits of its own",
    };
    let next = match summary {
        Some(path) => format!(
            "Triage its summary at {}, then delete the file.",
            path.display()
        ),
        None => "It left no summary.".to_string(),
    };
    format!("Feature '{name}' {what}. {next}")
}

/// Remove a feature's worktree, branch, state file, agent registry,
/// message queue, and tmux session. Returns warnings: what it could not
/// remove without failing the rest.
///
/// The tmux session is killed last so that cleanup completes even when run
/// from within the feature session (where killing the session would kill
/// this process).
pub fn cleanup_feature(params: &CleanupParams) -> Result<Vec<String>> {
    let mut tlog = params
        .features_dir
        .parent()
        .map(|pm_dir| TimingLog::new(pm_dir, "cleanup", params.name));

    cleanup_feature_with_timing(params, &mut tlog)
}

/// Run cleanup with an optional timing log. The log is flushed before
/// the session-killing step so timings survive even if the process is
/// terminated. Called directly by `feat_merge` to share a single log.
pub(crate) fn cleanup_feature_with_timing(
    params: &CleanupParams,
    tlog: &mut Option<TimingLog>,
) -> Result<Vec<String>> {
    let cleanup_start = Instant::now();
    let mut warnings = Vec::new();

    /// Run a cleanup step, recording its duration in `$tlog` and handling
    /// best-effort error swallowing. This is a macro rather than a closure
    /// so that `$tlog` can be mutably borrowed across multiple invocations
    /// without conflicting with later borrows (e.g. for recording totals).
    macro_rules! run {
        ($tlog:expr, $label:expr, $best_effort:expr, $step:expr) => {{
            let t = Instant::now();
            #[allow(clippy::redundant_closure_call)]
            let result: Result<()> = (|| $step)();
            let elapsed = t.elapsed();
            if let Some(tl) = $tlog.as_mut() {
                tl.record($label, elapsed);
            }
            match result {
                Ok(()) => Ok(()),
                Err(e) if $best_effort => {
                    eprintln!("warning: cleanup step failed (continuing): {e}");
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }};
    }

    // Step 0: Keep the summary while the worktree that may hold a legacy one
    // still exists.
    let mut summary = None;
    run!(tlog, "collect-summary", params.best_effort, {
        if params.ending.is_some()
            && let Some(project_root) = params.features_dir.parent().and_then(Path::parent)
        {
            summary = crate::commands::feat_summary::collect(
                project_root,
                params.worktree_path,
                params.name,
            )?;
        }
        Ok(())
    })?;

    // Step 1: Remove git worktree
    run!(
        tlog,
        "remove-worktree",
        params.best_effort,
        remove_worktree(params, &mut warnings)
    )?;

    // Step 1b: Prune stale worktree entries so git no longer considers the
    // branch checked-out. Without this, `git branch -D` can race against the
    // worktree bookkeeping update from step 1.
    run!(
        tlog,
        "prune-worktrees",
        params.best_effort,
        git::prune_worktrees(params.repo)
    )?;

    // Step 2: Delete branch (skipped during feat_adopt rollback)
    run!(tlog, "delete-branch", params.best_effort, {
        if params.delete_branch && git::branch_exists(params.repo, params.branch)? {
            git::delete_branch(params.repo, params.branch)?;
        }
        Ok(())
    })?;

    // Step 3: Remove state file
    run!(
        tlog,
        "delete-state",
        params.best_effort,
        FeatureState::delete(params.features_dir, params.name)
    )?;

    // Step 4: Remove agent registry and message queue.
    // Derive .pm/ dir from features_dir (which is <project_root>/.pm/features/).
    run!(tlog, "delete-agents-messages", params.best_effort, {
        if let Some(pm_dir) = params.features_dir.parent() {
            let agents_dir = pm_dir.join("agents");
            AgentRegistry::delete(&agents_dir, params.name)?;

            let messages_dir = pm_dir.join("messages");
            messages::delete_feature(&messages_dir, params.name)?;
            if let Some(project_root) = pm_dir.parent() {
                crate::state::runtime::remove_scope(project_root, params.name)?;
            }
        }
        Ok(())
    })?;

    // Step 4.5: Tell main how the feature ended, before killing the session
    // (the session kill terminates this process if run from within the
    // feature session)
    run!(tlog, "notify-main", params.best_effort, {
        if let Some(ending) = params.ending
            && let Some(pm_dir) = params.features_dir.parent()
        {
            messages::send_with_scope(
                &pm_dir.join("messages"),
                "main",
                "main",
                params.name,
                &ending_body(params.name, ending, summary.as_deref()),
                Some(params.name),
            )?;
        }
        Ok(())
    })?;

    // Flush timing log before killing the session — the session kill may
    // terminate this process, so we must persist timings first.
    if let Some(tl) = tlog.as_mut() {
        tl.record_total(cleanup_start.elapsed());
        tl.flush();
    }

    // Step 5: Kill tmux session (last — see doc comment above)
    run!(tlog, "kill-session", params.best_effort, {
        let session_name = tmux::session_name(params.project_name, params.name);
        if params.kill_session && tmux::has_session(params.tmux_server, &session_name)? {
            let base_session = tmux::session_name(params.project_name, params.base_scope);
            tmux::clients::move_off(
                params.tmux_server,
                std::slice::from_ref(&session_name),
                Some(&base_session),
            )?;
            tmux::kill_session(params.tmux_server, &session_name)?;
        }
        Ok(())
    })?;

    Ok(warnings)
}

/// Remove the feature's worktree. A directory git no longer knows as one
/// (see [`git::is_worktree`]) that pm created goes from disk directly; what
/// of it can't is a warning, so the feature's state still goes.
fn remove_worktree(params: &CleanupParams, warnings: &mut Vec<String>) -> Result<()> {
    let path = params.worktree_path;
    if !path.exists() {
        return Ok(());
    }
    let tracked = git::is_worktree(params.repo, path)?;
    if tracked || !params.worktree_created {
        let removed = if params.force_worktree {
            git::remove_worktree_force(params.repo, path)
        } else {
            git::remove_worktree(params.repo, path)
        };
        match removed {
            Ok(()) => return Ok(()),
            Err(e) if !params.worktree_created || git::is_worktree(params.repo, path)? => {
                return Err(e);
            }
            Err(_) => {}
        }
    }
    match std::fs::remove_dir_all(path) {
        Err(e) => warnings.push(format!(
            "could not remove {} ({e}): delete what is left of it by hand",
            path.display()
        )),
        Ok(()) if !tracked => warnings.push(format!(
            "{} was no longer a git worktree; removed what was left of it",
            path.display()
        )),
        Ok(()) => {}
    }
    Ok(())
}
