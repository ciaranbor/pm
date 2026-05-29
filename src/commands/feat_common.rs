//! Shared helpers for feature-creation commands (feat_new, feat_adopt, feat_review).
//!
//! Each creation flow is a linear recipe with small but meaningful divergences,
//! so we expose plain helper functions rather than a builder or trait. Each
//! helper captures a step that was byte-for-byte duplicated across the three
//! flows; call sites continue to read as inspectable recipes.

use std::path::Path;

use chrono::Utc;

use crate::commands::{agent_spawn, feat_delete};
use crate::error::Result;
use crate::messages;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::workflow::WorkflowDef;

/// Fields needed to write an Initializing-status feature state file.
pub struct InitStateFields<'a> {
    pub branch: &'a str,
    pub worktree: &'a str,
    pub base: &'a str,
    pub pr: &'a str,
    pub context: &'a str,
    pub workflow: Option<&'a str>,
}

/// Write a feature state file with `status = Initializing` and current timestamps.
/// Returns the populated `FeatureState` so the caller can mutate it later
/// (e.g. flip status to `Wip`/`Review` and re-save once setup succeeds).
pub fn write_initializing_state(
    features_dir: &Path,
    name: &str,
    fields: InitStateFields<'_>,
) -> Result<FeatureState> {
    let now = Utc::now();
    let state = FeatureState {
        status: FeatureStatus::Initializing,
        branch: fields.branch.to_string(),
        worktree: fields.worktree.to_string(),
        base: fields.base.to_string(),
        pr: fields.pr.to_string(),
        context: fields.context.to_string(),
        workflow: fields.workflow.map(|s| s.to_string()),
        created: now,
        last_active: now,
    };
    state.save(features_dir, name)?;
    Ok(state)
}

/// Enqueue a feature's initial context as a message in each `auto_spawn`
/// agent's inbox. The pm Stop hook will deliver it on each agent's empty
/// first turn. Caller passes the workflow's loaded `auto_spawn` list; the
/// empty case (no auto-spawn agents) is handled silently.
pub fn enqueue_initial_context(
    project_root: &Path,
    feature_name: &str,
    auto_spawn: &[String],
    context: &str,
    base_scope: &str,
) -> Result<()> {
    if auto_spawn.is_empty() {
        return Ok(());
    }
    let messages_dir = paths::messages_dir(project_root);
    for agent in auto_spawn {
        // Record sender_scope so that `pm msg reply` routes the response
        // back to the correct scope (main, or a parent feature for stacked features).
        messages::send_with_scope(
            &messages_dir,
            feature_name,
            agent,
            base_scope,
            context,
            Some(base_scope),
        )?;
    }
    Ok(())
}

/// Spawn each `auto_spawn` agent for a newly-created feature.
///
/// The first agent reuses `reuse_window` (typically window :0, the default
/// shell created by `tmux new-session`) to avoid leaving an empty window.
/// Subsequent agents are spawned into new windows.
///
/// The pm Stop hook is responsible for delivering any queued messages on
/// each agent's empty first turn — `spawn_claude_session` itself passes no
/// initial prompt.
pub fn spawn_auto_spawn_agents(
    project_root: &Path,
    feature_name: &str,
    auto_spawn: &[String],
    edit: bool,
    reuse_window: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<()> {
    for (idx, agent) in auto_spawn.iter().enumerate() {
        // Only the first agent reuses the default shell window. All
        // subsequent agents get their own fresh window.
        let reuse = if idx == 0 { reuse_window } else { None };
        agent_spawn::spawn_claude_session(&agent_spawn::SpawnClaudeParams {
            project_root,
            feature: feature_name,
            agent_name: Some(agent.as_str()),
            // Workflow auto-spawn has no concept of aliasing — the
            // workflow's `auto_spawn` entry doubles as the definition.
            // `spawn_claude_session` falls back to `agent_name` when this
            // is `None`.
            agent_definition: None,
            prompt: None,
            edit,
            resume_session: None,
            fork_session: false,
            reuse_window: reuse,
            tmux_server,
        })?;
    }
    Ok(())
}

/// Convenience: load a workflow def, validate `auto_spawn` agents exist,
/// and return the loaded def. Errors propagate from both steps so callers
/// can surface the workflow problem before any filesystem side effects.
pub fn load_and_validate_workflow(project_root: &Path, name: &str) -> Result<WorkflowDef> {
    let def = WorkflowDef::load(project_root, name)?;
    def.validate_auto_spawn(project_root, name)?;
    Ok(def)
}

/// Parameters for rolling back a partial feature creation.
pub struct RollbackParams<'a> {
    pub project_root: &'a Path,
    pub feature_name: &'a str,
    /// The git branch name (may differ from `feature_name` when slashes are sanitized).
    pub branch: &'a str,
    pub project_name: &'a str,
    pub tmux_server: Option<&'a str>,
    /// Whether to delete the branch. Set to `false` for `feat_adopt` (user-owned branch).
    pub delete_branch: bool,
    /// The base worktree name (e.g. "main" or a parent feature name).
    pub base: &'a str,
}

/// Best-effort rollback of a partial feature creation. Thin wrapper around
/// `feat_delete::cleanup_feature` in `best_effort` mode, so every cleanup step
/// (worktree removal, state file, agent registry, message queue, tmux
/// session) runs even if an earlier one fails.
pub fn rollback_creation(params: &RollbackParams<'_>) {
    let base_worktree = params.project_root.join(params.base);
    let worktree_path = params.project_root.join(params.feature_name);
    let features_dir = paths::features_dir(params.project_root);

    let _ = feat_delete::cleanup_feature(&feat_delete::CleanupParams {
        repo: &base_worktree,
        worktree_path: &worktree_path,
        branch: params.branch,
        features_dir: &features_dir,
        name: params.feature_name,
        project_name: params.project_name,
        force_worktree: true,
        tmux_server: params.tmux_server,
        delete_branch: params.delete_branch,
        best_effort: true,
        base: params.base,
    });
}
