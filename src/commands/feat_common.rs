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
use crate::state::project::ProjectConfig;

/// Fields needed to write an Initializing-status feature state file.
pub struct InitStateFields<'a> {
    pub branch: &'a str,
    pub worktree: &'a str,
    pub base: &'a str,
    pub pr: &'a str,
    pub context: &'a str,
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
        created: now,
        last_active: now,
    };
    state.save(features_dir, name)?;
    Ok(state)
}

/// Resolve the agent to spawn for a feature-creation flow: explicit override
/// first, then project default, then plain claude (None).
pub fn resolve_default_agent<'a>(
    agent_override: Option<&'a str>,
    config: &'a ProjectConfig,
) -> Option<&'a str> {
    agent_override.or_else(|| {
        let d = &config.agents.default;
        if d.is_empty() { None } else { Some(d.as_str()) }
    })
}

/// Enqueue a feature's initial context as a message in the resolved default
/// agent's inbox. The pm Stop hook will deliver it on the agent's empty first
/// turn. Returns the name of the agent the message was queued for (if any).
///
/// With no default agent configured and no override, no message is enqueued
/// — plain claude sessions don't have an inbox keyed by a named agent.
pub fn enqueue_initial_context<'a>(
    project_root: &Path,
    feature_name: &str,
    config: &'a ProjectConfig,
    agent_override: Option<&'a str>,
    context: &str,
) -> Result<Option<&'a str>> {
    let Some(agent) = resolve_default_agent(agent_override, config) else {
        return Ok(None);
    };
    let messages_dir = paths::messages_dir(project_root);
    let sender = messages::default_user_name();
    messages::send(&messages_dir, feature_name, agent, &sender, context)?;
    Ok(Some(agent))
}

/// Spawn the default claude agent for a newly-created feature: resolves
/// override → config default → plain claude, then calls
/// `agent_spawn::spawn_claude_session` with no initial prompt. The pm Stop
/// hook is responsible for delivering any queued message on the empty first
/// turn.
///
/// Only used by feat_new and feat_adopt. feat_review bypasses this because it
/// always spawns the hardcoded `reviewer` agent in read-only mode.
pub fn spawn_default_agent(
    project_root: &Path,
    feature_name: &str,
    config: &ProjectConfig,
    agent_override: Option<&str>,
    no_edit: bool,
    tmux_server: Option<&str>,
) -> Result<()> {
    let agent = resolve_default_agent(agent_override, config);
    agent_spawn::spawn_claude_session(
        project_root,
        feature_name,
        agent,
        None,
        !no_edit,
        None,
        tmux_server,
    )?;
    Ok(())
}

/// Best-effort rollback of a partial feature creation. Thin wrapper around
/// `feat_delete::cleanup_feature` in `best_effort` mode, so every cleanup step
/// (worktree removal, state file, agent registry, message queue, tmux
/// session) runs even if an earlier one fails. Use `delete_branch = false`
/// from `feat_adopt`, which must never destroy a user-owned branch.
///
/// `branch` is passed separately from `feature_name` because they can differ:
/// `feat_new` may receive a slash-containing branch that sanitizes to a
/// different feature name, while `feat_review` uses the sanitized feature
/// name as both branch and feature id.
pub fn rollback_creation(
    project_root: &Path,
    feature_name: &str,
    branch: &str,
    project_name: &str,
    tmux_server: Option<&str>,
    delete_branch: bool,
    base: &str,
) {
    let base_worktree = project_root.join(base);
    let worktree_path = project_root.join(feature_name);
    let features_dir = paths::features_dir(project_root);

    let _ = feat_delete::cleanup_feature(&feat_delete::CleanupParams {
        repo: &base_worktree,
        worktree_path: &worktree_path,
        branch,
        features_dir: &features_dir,
        name: feature_name,
        project_name,
        force_worktree: true,
        tmux_server,
        delete_branch,
        best_effort: true,
        base,
    });
}
