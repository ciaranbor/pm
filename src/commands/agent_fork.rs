use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::agent::AgentRegistry;
use crate::state::paths;

use super::agent_spawn::{SpawnClaudeParams, spawn_claude_session};

/// Fork an existing agent: spawn a new agent that starts with a copy of
/// the source's conversation history.
///
/// Implemented via Claude Code's built-in `--fork-session` flag, which
/// loads the source's transcript and assigns the resumed conversation a
/// fresh session id. The source's session file is left untouched, so the
/// source can keep running and the two histories diverge cleanly from
/// the moment of the fork.
///
/// Errors if:
/// - `source` does not exist in the registry
/// - `source` has no `session_id` (nothing to fork from)
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
        return Err(PmError::Agent(format!(
            "source and new agent names must differ ('{source}')"
        )));
    }

    let agents_dir = paths::agents_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;

    let source_entry = registry.get(source).ok_or_else(|| {
        PmError::AgentNotFound(format!("'{source}' not found in scope '{feature}'"))
    })?;

    if source_entry.session_id.is_empty() {
        return Err(PmError::Agent(format!(
            "agent '{source}' has no session_id to fork from"
        )));
    }

    if registry.get(new_name).is_some() {
        return Err(PmError::Agent(format!(
            "agent '{new_name}' already exists in scope '{feature}'"
        )));
    }

    let source_session_id = source_entry.session_id.clone();

    let window_target = spawn_claude_session(&SpawnClaudeParams {
        project_root,
        feature,
        agent_name: Some(new_name),
        prompt: None,
        edit: false,
        resume_session: Some(&source_session_id),
        fork_session: true,
        reuse_window: None,
        tmux_server,
    })?;

    Ok(format!(
        "Forked '{source}' as '{new_name}' in {window_target}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_spawn;
    use crate::state::agent::{AgentEntry, AgentType};
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::state::project::ProjectConfig;
    use crate::testing::TestServer;
    use crate::tmux;
    use chrono::Utc;
    use tempfile::tempdir;

    fn setup_project(dir: &Path, server: &TestServer) -> (String, String) {
        let root = dir.to_path_buf();
        let pm_dir = root.join(".pm");
        let project_name = server.scope("proj");
        let feature_name = "login";

        std::fs::create_dir_all(pm_dir.join("features")).unwrap();

        let config = ProjectConfig {
            project: crate::state::project::ProjectInfo {
                name: project_name.clone(),
                max_features: None,
            },
            setup: Default::default(),
            github: Default::default(),
            agents: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        let now = Utc::now();
        let state = FeatureState {
            status: FeatureStatus::Wip,
            branch: feature_name.to_string(),
            worktree: feature_name.to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            created: now,
            last_active: now,
        };
        state.save(&pm_dir.join("features"), feature_name).unwrap();

        let worktree = root.join(feature_name);
        std::fs::create_dir_all(&worktree).unwrap();

        let session_name = tmux::session_name(&project_name, feature_name);
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        (session_name, feature_name.to_string())
    }

    /// Spawn a source agent and stamp a session_id (simulating what
    /// claude's SessionStart hook would do).
    fn spawn_source_with_session_id(
        dir: &Path,
        feature: &str,
        name: &str,
        session_id: &str,
        server: &TestServer,
    ) {
        agent_spawn::agent_spawn(dir, feature, name, None, false, server.name()).unwrap();
        let agents_dir = paths::agents_dir(dir);
        let mut registry = AgentRegistry::load(&agents_dir, feature).unwrap();
        registry.get_mut(name).unwrap().session_id = session_id.to_string();
        registry.save(&agents_dir, feature).unwrap();
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

        agent_spawn::agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name())
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
    fn fork_errors_if_new_name_already_exists() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        spawn_source_with_session_id(dir.path(), &feature, "reviewer", "src-session-id", &server);

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.register(
            "reviewer-2",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer-2".to_string(),
                active: true,
            },
        );
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
