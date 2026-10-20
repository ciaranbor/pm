//! `pm agent fork`: a new agent that starts with a copy of another's
//! conversation, through the harness's own fork (Claude Code's
//! `--fork-session`, `codex fork`, opencode's `session.fork`), which loads
//! the source's transcript under a fresh session id. The source's session
//! is left untouched, so the source can keep running and the two histories
//! diverge from the moment of the fork. A transcript exists only on the
//! harness that wrote it, so a source whose configured harness has changed
//! is refused.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig};

use super::agent_spawn::{SpawnParams, notes_suffix, spawn_session};

/// Fork an existing agent: spawn a new agent that starts with a copy of
/// the source's conversation history.
///
/// Errors if:
/// - the scope's tmux session is not running
/// - `source` does not exist in the registry
/// - `source` has no `session_id`, or its harness has no conversation for
///   it (a session that never took a turn): nothing to fork from
/// - `new_name` already exists in the registry
pub fn agent_fork(
    project_root: &Path,
    feature: &str,
    source: &str,
    new_name: &str,
    tmux_server: Option<&str>,
) -> Result<String> {
    crate::messages::validate_name(source, "agent")?;
    crate::messages::validate_name(new_name, "agent")?;

    if source == new_name {
        return Err(PmError::Unforkable(format!(
            "source and new agent names must differ ('{source}')"
        )));
    }

    let config = ProjectConfig::load(&paths::pm_dir(project_root))?;
    super::agent_spawn::require_session(&config.project.name, feature, tmux_server)?;

    let agents_dir = paths::agents_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;

    let source_entry = registry.require(source, feature)?;

    if source_entry.session_id.is_empty() {
        return Err(PmError::Unforkable(format!(
            "agent '{source}' has no session_id to fork from"
        )));
    }

    if registry.get(new_name).is_some() {
        return Err(PmError::Unforkable(format!(
            "agent '{new_name}' already exists in scope '{feature}'"
        )));
    }

    let source_session_id = source_entry.session_id.clone();
    // Inherit the source's claude agent definition so the fork is launched
    // with the same `--agent <def>` flag. When the source has no explicit
    // definition stored, fall back to the source's display name (the
    // back-compat behaviour). The forked entry stores its own
    // `agent_definition` if it differs from `new_name`, so subsequent
    // restart/resume on the fork uses the correct definition too.
    let inherited_definition: String = source_entry
        .agent_definition
        .clone()
        .unwrap_or_else(|| source.to_string());

    // `fork` bypasses `agent_spawn`'s validation chokepoint — re-check the
    // inherited def here (valid at spawn, but may have been removed since).
    super::agent_spawn::validate_definition_resolves(project_root, &inherited_definition)?;

    // The source's transcript only exists on the harness that wrote it; a
    // fork onto a different harness would be a fresh agent, not a fork.
    let global = GlobalConfig::load_or_default();
    let harness = super::agent_spawn::configured_harness(
        &inherited_definition,
        &config.agents,
        &global.agents,
    )?;
    if harness != source_entry.harness {
        return Err(PmError::Unforkable(format!(
            "agent '{source}' ran on harness {}, but config now selects {harness} for \
             '{inherited_definition}' — its session cannot be forked across harnesses",
            source_entry.harness
        )));
    }

    if harness.resume_needs_conversation()
        && source_entry
            .conversation(project_root, feature, source)?
            .is_none()
    {
        return Err(PmError::Unforkable(format!(
            "agent '{source}' has no conversation to fork: {harness} has none for session {} \
             (it never took a turn); spawn a new agent instead",
            source_entry.session_id
        )));
    }

    let spawned = spawn_session(&SpawnParams {
        project_root,
        feature,
        agent_name: new_name,
        agent_definition: Some(&inherited_definition),
        prompt: None,
        resume_session: Some(&source_session_id),
        fork_session: true,
        reuse_window: None,
        tmux_server,
    })?;

    Ok(format!(
        "Forked '{source}' as '{new_name}' in {}{}",
        spawned.window_target,
        notes_suffix(&spawned.notes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_spawn;
    use crate::commands::agent_spawn::test_support::setup_project;
    use crate::harness::Harness;
    use crate::state::agent::AgentType;
    use crate::state::project::ProjectConfig;
    use crate::testing::{TestServer, agent_entry};
    use crate::tmux;
    use tempfile::tempdir;

    /// Spawn a source agent and record its session with a conversation
    /// (what claude's SessionStart hook and first turn would leave).
    fn spawn_source_with_session_id(
        dir: &Path,
        feature: &str,
        name: &str,
        session_id: &str,
        server: &TestServer,
    ) {
        agent_spawn::agent_spawn(dir, feature, name, None, None, server.name()).unwrap();
        agent_spawn::test_support::record_session(dir, feature, name, session_id);
    }

    #[test]
    fn fork_creates_new_agent_with_window() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        spawn_source_with_session_id(dir.path(), &feature, "reviewer", "src-session-id", &server);

        let msg = agent_fork(
            dir.path(),
            &feature,
            "reviewer",
            "reviewer-2",
            server.name(),
        )
        .unwrap();
        assert!(msg.contains("Forked 'reviewer' as 'reviewer-2'"));

        // New tmux window exists alongside the source
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer-2")
                .unwrap()
                .is_some()
        );
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some(),
            "source window must remain"
        );

        // New agent registered with active = true; source still present
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get("reviewer-2").unwrap();
        assert_eq!(entry.agent_type, AgentType::Agent);
        assert!(entry.active);
        assert!(registry.get("reviewer").is_some());
    }

    #[test]
    fn fork_works_while_source_is_running() {
        // Regression for the rejected "stop the source first" design.
        // With `--fork-session` the source's session file is untouched,
        // so the source can stay running.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        spawn_source_with_session_id(dir.path(), &feature, "reviewer", "src-session-id", &server);

        // Source's window is alive (spawn_source_with_session_id spawned it).
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );

        agent_fork(
            dir.path(),
            &feature,
            "reviewer",
            "reviewer-2",
            server.name(),
        )
        .unwrap();

        // Both windows exist
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer-2")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn fork_inherits_agent_definition_from_source() {
        // When the source was spawned with an alias (e.g. display name
        // 'frontend-dev', definition 'implementer'), the fork must inherit
        // the definition so its `--agent` flag matches the source's.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Spawn aliased source and stamp a session_id.
        agent_spawn::agent_spawn(
            dir.path(),
            &feature,
            "frontend-dev",
            Some("implementer"),
            None,
            server.name(),
        )
        .unwrap();
        let agents_dir = paths::agents_dir(dir.path());
        agent_spawn::test_support::record_session(
            dir.path(),
            &feature,
            "frontend-dev",
            "src-session-id",
        );

        agent_fork(
            dir.path(),
            &feature,
            "frontend-dev",
            "frontend-dev-2",
            server.name(),
        )
        .unwrap();

        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let fork_entry = registry.get("frontend-dev-2").unwrap();
        // Fork inherits the source's stored definition
        assert_eq!(fork_entry.agent_definition.as_deref(), Some("implementer"));
    }

    #[test]
    fn fork_refuses_a_session_that_never_took_a_turn() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        agent_spawn::agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap();
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.get_mut("reviewer").unwrap().session_id = "sess-unturned".to_string();
        registry.save(&agents_dir, &feature).unwrap();

        let err = agent_fork(
            dir.path(),
            &feature,
            "reviewer",
            "reviewer-2",
            server.name(),
        )
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            PmError::Unforkable(
                "agent 'reviewer' has no conversation to fork: claude-code has none for session \
                 sess-unturned (it never took a turn); spawn a new agent instead"
                    .into()
            )
            .to_string()
        );
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("reviewer-2").is_none());
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer-2")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn fork_refuses_while_the_scopes_session_is_down() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        tmux::kill_session(server.name(), &session_name).unwrap();

        let err = agent_fork(
            dir.path(),
            &feature,
            "reviewer",
            "reviewer-2",
            server.name(),
        )
        .unwrap_err();

        assert!(matches!(err, PmError::SessionNotRunning { .. }), "{err}");
    }

    #[test]
    fn fork_errors_if_source_missing() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_fork(dir.path(), &feature, "ghost", "reviewer-2", server.name());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::AgentNotFound(_)));
    }

    #[test]
    fn fork_errors_if_source_has_no_session_id() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn::agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap();

        let result = agent_fork(
            dir.path(),
            &feature,
            "reviewer",
            "reviewer-2",
            server.name(),
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("no session_id"), "got: {err}");
    }

    #[test]
    fn fork_on_opencode_opens_the_session_opencode_forked() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        let pm_dir = paths::pm_dir(dir.path());
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".to_string(), "opencode".to_string());
        config
            .agents
            .models
            .insert("reviewer".to_string(), "local/qwen".to_string());
        agent_spawn::test_support::project_opencode_definition(
            &dir.path().join(&feature),
            "reviewer",
        );
        config.harness.opencode.binary = Some(crate::testing::fake_opencode(
            dir.path(),
            r#"{"data":{"id":"ses_source"}}"#,
            0,
        ));
        config.save(&pm_dir).unwrap();
        agent_spawn::agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap();

        crate::testing::fake_opencode(dir.path(), r#"{"data":{"id":"ses_fork"}}"#, 0);
        agent_fork(
            dir.path(),
            &feature,
            "reviewer",
            "reviewer-2",
            server.name(),
        )
        .unwrap();

        let registry = AgentRegistry::load(&paths::agents_dir(dir.path()), &feature).unwrap();
        assert_eq!(registry.get("reviewer").unwrap().session_id, "ses_source");
        let fork = registry.get("reviewer-2").unwrap();
        assert_eq!(fork.session_id, "ses_fork");
        assert_eq!(fork.harness, Harness::OpenCode);
        assert_eq!(fork.agent_definition.as_deref(), Some("reviewer"));
        let target = tmux::find_window(server.name(), &session_name, "reviewer-2")
            .unwrap()
            .expect("window");
        server.wait_for_pane_text(&target, "'PM_OPENCODE_SESSION=ses_fork'");
    }

    #[test]
    fn fork_on_opencode_reports_why_opencode_refused_and_registers_nothing() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        let pm_dir = paths::pm_dir(dir.path());
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".to_string(), "opencode".to_string());
        config
            .agents
            .models
            .insert("reviewer".to_string(), "local/qwen".to_string());
        agent_spawn::test_support::project_opencode_definition(
            &dir.path().join(&feature),
            "reviewer",
        );
        config.harness.opencode.binary = Some(crate::testing::fake_opencode(
            dir.path(),
            r#"{"data":{"id":"ses_source"}}"#,
            0,
        ));
        config.save(&pm_dir).unwrap();
        agent_spawn::agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap();

        // What opencode answers for a source that has not had a turn yet.
        crate::testing::fake_opencode(
            dir.path(),
            "{\"_tag\":\"InvalidRequestError\",\"message\":\"Cannot fork empty session: \
             ses_source\",\"kind\":\"empty_session\"}\nHTTP 400 Bad Request",
            1,
        );
        let err = agent_fork(
            dir.path(),
            &feature,
            "reviewer",
            "reviewer-2",
            server.name(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("opencode session.fork failed: Cannot fork empty session: ses_source"),
            "{err}"
        );
        let registry = AgentRegistry::load(&paths::agents_dir(dir.path()), &feature).unwrap();
        assert!(registry.get("reviewer-2").is_none());
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer-2")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn fork_refuses_to_cross_harnesses() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        spawn_source_with_session_id(dir.path(), &feature, "reviewer", "src-session-id", &server);

        let pm_dir = paths::pm_dir(dir.path());
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".to_string(), "codex".to_string());
        config.save(&pm_dir).unwrap();

        let err = agent_fork(
            dir.path(),
            &feature,
            "reviewer",
            "reviewer-2",
            server.name(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("ran on harness claude-code, but config now selects codex"),
            "got: {err}"
        );
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer-2")
                .unwrap()
                .is_none()
        );
        let registry = AgentRegistry::load(&paths::agents_dir(dir.path()), &feature).unwrap();
        assert!(registry.get("reviewer-2").is_none());
    }

    #[test]
    fn fork_errors_if_new_name_already_exists() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        spawn_source_with_session_id(dir.path(), &feature, "reviewer", "src-session-id", &server);

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.register("reviewer-2", agent_entry("reviewer-2"));
        registry.save(&agents_dir, &feature).unwrap();

        let result = agent_fork(
            dir.path(),
            &feature,
            "reviewer",
            "reviewer-2",
            server.name(),
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("already exists"), "got: {err}");
    }

    #[test]
    fn fork_errors_if_source_equals_new_name() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        spawn_source_with_session_id(dir.path(), &feature, "reviewer", "src-session-id", &server);

        let result = agent_fork(dir.path(), &feature, "reviewer", "reviewer", server.name());
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("must differ"), "got: {err}");
    }

    #[test]
    fn fork_rejects_invalid_names() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_fork(dir.path(), &feature, "../evil", "reviewer-2", server.name());
        assert!(result.is_err());

        let result = agent_fork(dir.path(), &feature, "reviewer", "foo:bar", server.name());
        assert!(result.is_err());
    }
}
