//! Shared feature helpers, mostly for the creation commands (feat_new, feat_adopt, feat_review).
//!
//! Each creation flow is a linear recipe with small but meaningful divergences,
//! so we expose plain helper functions rather than a builder or trait. Each
//! helper captures a step that was byte-for-byte duplicated across the three
//! flows; call sites continue to read as inspectable recipes.
//!
//! The team-brief contract of `feat new` and `feat adopt --workflow`: the
//! whole team spawns, refused up front if a member's harness can't run the
//! workflow ([`load_and_validate_workflow`]), and the brief goes only to the
//! workflow's `brief_agents` ([`enqueue_initial_context`]); a brief with
//! none to receive it is an error.

use std::path::Path;

use chrono::Utc;

use crate::commands::bundled_disable::Disabled;
use crate::commands::{agent_spawn, feat_delete, harness_check};
use crate::error::{PmError, Result};
use crate::messages;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::workflow::WorkflowDef;

/// Workflow used when `--context` is given without `--workflow`: a brief
/// needs a recipient, and `solo` is the single-agent default team.
pub const DEFAULT_WORKFLOW: &str = "solo";

/// Resolve the effective workflow for a feature-creation command.
///
/// Explicit `--workflow` always wins. Otherwise, a `--context` with no
/// workflow defaults to [`DEFAULT_WORKFLOW`] (checked to be installed so
/// the failure is actionable, not a generic not-found). No context and no
/// workflow stays agentless.
pub fn resolve_workflow<'a>(
    project_root: &Path,
    workflow: Option<&'a str>,
    context: Option<&str>,
) -> Result<Option<&'a str>> {
    resolve_workflow_in(
        project_root,
        workflow,
        context,
        &crate::state::workflow::global_dir()?,
        &Disabled::load(),
    )
}

/// [`resolve_workflow`] against an explicit global workflow tier.
pub fn resolve_workflow_in<'a>(
    project_root: &Path,
    workflow: Option<&'a str>,
    context: Option<&str>,
    global_dir: &Path,
    disabled: &Disabled,
) -> Result<Option<&'a str>> {
    match (workflow, context) {
        (Some(w), _) => Ok(Some(w)),
        (None, Some(_)) => {
            if crate::state::workflow::resolve_dir(Some(project_root), DEFAULT_WORKFLOW, global_dir)
                .is_none()
            {
                if disabled.workflow(DEFAULT_WORKFLOW) {
                    return Err(disabled.explain(
                        project_root,
                        PmError::WorkflowNotFound(DEFAULT_WORKFLOW.to_string()),
                    ));
                }
                return Err(PmError::SafetyCheck(format!(
                    "default workflow '{DEFAULT_WORKFLOW}' is not installed. \
                     Run `pm upgrade`, or pass --workflow <name>."
                )));
            }
            Ok(Some(DEFAULT_WORKFLOW))
        }
        (None, None) => Ok(None),
    }
}

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
        team: Default::default(),
    };
    state.save(features_dir, name)?;
    Ok(state)
}

/// Enqueue a feature's initial context as a message in each `brief_agents`
/// agent's inbox. pm's waiter wakes each agent with it once its session
/// starts. Caller passes the workflow's loaded `brief_agents` list; the
/// empty case (no brief recipients) is handled silently.
///
/// The brief is sent with no sender scope and a `no-reply-brief` sender, so
/// `pm msg read` shows no reply hint and the agent has no `main` reply
/// target — the feature→project channel is the summary, not a reply.
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
/// pm's waiter delivers any queued messages once each agent's session
/// starts — `spawn_session` itself passes no initial prompt.
///
/// Config notes are printed per agent here: the feature commands report
/// only the feature name, so there is no per-agent status line to carry them.
pub fn spawn_team(
    project_root: &Path,
    feature_name: &str,
    team: &[String],
    reuse_window: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<()> {
    for (idx, agent) in team.iter().enumerate() {
        // Only the first agent reuses the default shell window. All
        // subsequent agents get their own fresh window.
        let reuse = if idx == 0 { reuse_window } else { None };
        let spawned = agent_spawn::spawn_session(&agent_spawn::SpawnParams {
            project_root,
            feature: feature_name,
            agent_name: agent.as_str(),
            // Workflow team spawn has no concept of aliasing — the
            // workflow's `agents` entry doubles as the definition.
            // `spawn_session` falls back to `agent_name` when this
            // is `None`.
            agent_definition: None,
            prompt: None,
            resume_session: None,
            fork_session: false,
            reuse_window: reuse,
            tmux_server,
        })?;
        for note in &spawned.notes {
            eprintln!("note: {agent}: {note}");
        }
    }
    Ok(())
}

/// Error unless `feature_name` is a registered feature of the project.
pub fn require_feature(project_root: &Path, feature_name: &str) -> Result<()> {
    if !FeatureState::exists(&paths::features_dir(project_root), feature_name) {
        return Err(PmError::FeatureNotFound(feature_name.to_string()));
    }
    Ok(())
}

/// Convenience: load a workflow def, validate its team agents exist, that
/// `brief_agents` is a subset of the team, and that each member's harness
/// can run it, and return the loaded def. Errors propagate from every step
/// so callers can surface the workflow problem before any filesystem side
/// effects.
pub fn load_and_validate_workflow(project_root: &Path, name: &str) -> Result<WorkflowDef> {
    let def = load_and_validate_team(
        project_root,
        name,
        &crate::state::workflow::global_dir()?,
        paths::home_dir().ok().as_deref(),
        &Disabled::load(),
    )?;
    harness_check::check_team(project_root, name, def.effective_team())?;
    Ok(def)
}

/// Load workflow `name` and validate its team against explicit tiers. A
/// workflow or member that fails to resolve because `[bundled.disable]` lists it
/// is reported as such.
fn load_and_validate_team(
    project_root: &Path,
    name: &str,
    global_dir: &Path,
    home: Option<&Path>,
    disabled: &Disabled,
) -> Result<WorkflowDef> {
    let explain = |e| disabled.explain(project_root, e);
    let def =
        WorkflowDef::load_with_global(Some(project_root), name, global_dir).map_err(explain)?;
    def.validate_with_home(project_root, name, home)
        .map_err(explain)?;
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
    /// The scope the client lands in if it was attached to the feature's
    /// session.
    pub base_scope: &'a str,
}

/// Best-effort rollback of a partial feature creation. Thin wrapper around
/// `feat_delete::cleanup_feature` in `best_effort` mode, so every cleanup step
/// (worktree removal, state file, agent registry, message queue, tmux
/// session) runs even if an earlier one fails.
pub fn rollback_creation(params: &RollbackParams<'_>) {
    let main_worktree = paths::main_worktree(params.project_root);
    let worktree_path = params.project_root.join(params.feature_name);
    let features_dir = paths::features_dir(params.project_root);

    let _ = feat_delete::cleanup_feature(&feat_delete::CleanupParams {
        repo: &main_worktree,
        worktree_path: &worktree_path,
        branch: params.branch,
        features_dir: &features_dir,
        name: params.feature_name,
        project_name: params.project_name,
        force_worktree: true,
        worktree_created: false,
        tmux_server: params.tmux_server,
        kill_session: true,
        delete_branch: params.delete_branch,
        best_effort: true,
        base_scope: params.base_scope,
        ending: None,
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

    #[test]
    fn context_without_workflow_defaults_to_solo_from_either_tier() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let global = tempdir().unwrap();

        // Nothing installed: the default is actionable rather than a
        // generic not-found.
        let err = resolve_workflow_in(
            project_root,
            None,
            Some("brief"),
            global.path(),
            &Disabled::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("'solo'") && err.contains("pm upgrade"),
            "{err}"
        );

        // Global tier alone is enough …
        let solo = global.path().join(DEFAULT_WORKFLOW);
        std::fs::create_dir_all(&solo).unwrap();
        std::fs::write(solo.join("config.toml"), "description = \"solo\"\n").unwrap();
        assert_eq!(
            resolve_workflow_in(
                project_root,
                None,
                Some("brief"),
                global.path(),
                &Disabled::default()
            )
            .unwrap(),
            Some(DEFAULT_WORKFLOW)
        );
        // … an explicit --workflow always wins, and no context stays agentless.
        assert_eq!(
            resolve_workflow_in(
                project_root,
                Some("other"),
                None,
                global.path(),
                &Disabled::default()
            )
            .unwrap(),
            Some("other")
        );
        assert_eq!(
            resolve_workflow_in(
                project_root,
                None,
                None,
                global.path(),
                &Disabled::default()
            )
            .unwrap(),
            None
        );
    }
    #[test]
    fn a_disabled_default_workflow_says_so_instead_of_pm_upgrade() {
        let dir = tempdir().unwrap();
        let global = tempdir().unwrap();
        let disabled = Disabled::from_config(toml::from_str("workflows = [\"solo\"]").unwrap());
        let err = resolve_workflow_in(dir.path(), None, Some("brief"), global.path(), &disabled)
            .unwrap_err();
        assert!(matches!(err, PmError::BundledDisabled { .. }), "{err}");
    }

    #[test]
    fn disabled_bundled_workflows_and_members_are_refused_unless_customs_resolve() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let global = tempdir().unwrap();
        let home = tempdir().unwrap();
        let disabled = Disabled::from_config(
            toml::from_str("agents = [\"qa\"]\nworkflows = [\"research-only\"]").unwrap(),
        );
        let load = |name| {
            load_and_validate_team(
                project_root,
                name,
                global.path(),
                Some(home.path()),
                &disabled,
            )
        };

        let err = load("research-only").unwrap_err();
        assert!(
            matches!(
                &err,
                PmError::BundledDisabled {
                    key: "workflows",
                    ..
                }
            ),
            "{err}"
        );

        let workflow = |name: &str| {
            let wf = paths::workflows_dir(project_root).join(name);
            std::fs::create_dir_all(&wf).unwrap();
            std::fs::write(
                wf.join("config.toml"),
                "description = \"d\"\nagents = [\"qa\"]\n",
            )
            .unwrap();
        };
        workflow("research-only");
        workflow("mine");
        for name in ["research-only", "mine"] {
            let err = load(name).unwrap_err();
            assert!(
                matches!(&err, PmError::BundledDisabled { key: "agents", .. }),
                "{name}: {err}"
            );
        }

        let agents = paths::main_worktree(project_root).join(".agents/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("qa.md"), "# my qa").unwrap();
        load("research-only").unwrap();
        load("mine").unwrap();
    }
}
