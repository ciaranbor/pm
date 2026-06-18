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

/// Enqueue a feature's initial context as a message in each `brief_agents`
/// agent's inbox. The pm Stop hook will deliver it on each agent's empty
/// first turn. Caller passes the workflow's loaded `brief_agents` list; the
/// empty case (no brief recipients) is handled silently.
///
/// The brief is sent with no sender scope and a `no-reply-brief` sender, so
/// `pm msg read` shows no reply hint and the agent has no `main` reply
/// target — the feature→project channel is `summary.md`, not a reply.
pub fn enqueue_initial_context(
    project_root: &Path,
    feature_name: &str,
    brief_agents: &[String],
    context: &str,
) -> Result<()> {
    if brief_agents.is_empty() {
        return Ok(());
    }
    let messages_dir = paths::messages_dir(project_root);
    for agent in brief_agents {
        messages::send(
            &messages_dir,
            feature_name,
            agent,
            "no-reply-brief",
            context,
        )?;
    }
    Ok(())
}

/// Spawn the workflow's full agent team for a newly-created feature.
///
/// The first agent reuses `reuse_window` (typically window :0, the default
/// shell created by `tmux new-session`) to avoid leaving an empty window.
/// Subsequent agents are spawned into new windows.
///
/// The pm Stop hook is responsible for delivering any queued messages on
/// each agent's empty first turn — `spawn_claude_session` itself passes no
/// initial prompt.
pub fn spawn_team(
    project_root: &Path,
    feature_name: &str,
    team: &[String],
    edit: bool,
    reuse_window: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<()> {
    for (idx, agent) in team.iter().enumerate() {
        // Only the first agent reuses the default shell window. All
        // subsequent agents get their own fresh window.
        let reuse = if idx == 0 { reuse_window } else { None };
        agent_spawn::spawn_claude_session(&agent_spawn::SpawnClaudeParams {
            project_root,
            feature: feature_name,
            agent_name: Some(agent.as_str()),
            // Workflow team spawn has no concept of aliasing — the
            // workflow's `agents` entry doubles as the definition.
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

/// Convenience: load a workflow def, validate its team agents exist and
/// that `brief_agents` is a subset of the team, and return the loaded def.
/// Errors propagate from both steps so callers can surface the workflow
/// problem before any filesystem side effects.
pub fn load_and_validate_workflow(project_root: &Path, name: &str) -> Result<WorkflowDef> {
    let def = WorkflowDef::load(project_root, name)?;
    def.validate(project_root, name)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_read;
    use tempfile::tempdir;

    #[test]
    fn brief_is_non_repliable() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let feature = "login";
        let brief_agents = vec!["implementer".to_string()];

        enqueue_initial_context(project_root, feature, &brief_agents, "do the thing").unwrap();

        // Sender is the no-reply sentinel, with no scope recorded.
        let messages_dir = paths::messages_dir(project_root);
        let summaries = messages::check(&messages_dir, feature, "implementer").unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].sender, "no-reply-brief");

        // Read output carries no `Reply:` hint — nothing to reply to.
        let out = agent_read::agent_read(project_root, feature, "implementer", None, None).unwrap();
        let joined = out.join("\n");
        assert!(joined.contains("do the thing"));
        assert!(
            !joined.contains("Reply:"),
            "feat-new brief must not show a reply hint, got: {joined}"
        );
    }
}
