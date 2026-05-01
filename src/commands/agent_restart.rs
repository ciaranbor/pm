use std::path::Path;

use crate::error::Result;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Restart a single agent: kill its tmux window, then respawn it.
/// The `active` flag stays `true` throughout. If the agent has a stored
/// `session_id`, passes `--resume` to claude on respawn. If the entry
/// records an explicit `agent_definition`, the definition is preserved
/// across the restart (relayed via `agent_spawn` reading the registry).
pub fn agent_restart(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    tmux_server: Option<&str>,
) -> Result<String> {
    crate::messages::validate_name(agent_name, "agent")?;

    let pm_dir = paths::pm_dir(project_root);
    let agents_dir = paths::agents_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let session_name = tmux::session_name(&config.project.name, feature);

    let registry = AgentRegistry::load(&agents_dir, feature)?;

    let entry = registry.get(agent_name).ok_or_else(|| {
        crate::error::PmError::AgentNotFound(format!(
            "'{agent_name}' not found in scope '{feature}'"
        ))
    })?;

    let resume_id = if entry.session_id.is_empty() {
        None
    } else {
        Some(entry.session_id.clone())
    };

    // Kill the existing tmux window if it exists
    if let Some(target) = tmux::find_window(tmux_server, &session_name, agent_name)? {
        let _ = tmux::kill_window(tmux_server, &target);
    }

    // Respawn via agent_spawn (which will see the registry entry, find no
    // window, and respawn). Passing `None` for `agent_definition` lets
    // `agent_spawn` re-read the stored definition from the registry, so
    // aliased agents keep their `--agent <def>` flag across restarts.
    // agent_spawn sets active = true on register, preserving the flag.
    // We discard agent_spawn's status message and craft our own, since
    // "Restarted ..." reads better than "Resumed ...".
    let (_outcome, _spawn_msg) = super::agent_spawn::agent_spawn(
        project_root,
        feature,
        agent_name,
        None,
        None,
        false,
        tmux_server,
    )?;

    let msg = if resume_id.is_some() {
        format!("Restarted agent '{agent_name}' (resumed session)")
    } else {
        format!("Restarted agent '{agent_name}'")
    };

    Ok(msg)
}

/// Restart multiple agents. Continues on error, returns all results.
pub fn agent_restart_many(
    project_root: &Path,
    feature: &str,
    names: &[String],
    tmux_server: Option<&str>,
) -> Vec<Result<String>> {
    names
        .iter()
        .map(|name| agent_restart(project_root, feature, name, tmux_server))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_spawn;
    use crate::state::agent::{AgentEntry, AgentType};
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::testing::TestServer;
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

    #[test]
    fn restart_respawns_agent_window() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn agent first
        agent_spawn::agent_spawn(
            dir.path(),
            &feature,
            "reviewer",
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        // Verify window exists
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );

        // Restart
        let msg = agent_restart(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("Restarted agent 'reviewer'"));

        // Window should still exist (new one)
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );

        // active flag should still be true
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("reviewer").unwrap().active);
    }

    #[test]
    fn restart_with_session_id_resumes() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn agent and set a session_id
        agent_spawn::agent_spawn(
            dir.path(),
            &feature,
            "reviewer",
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.get_mut("reviewer").unwrap().session_id = "sess-abc".to_string();
        registry.save(&agents_dir, &feature).unwrap();

        let msg = agent_restart(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("resumed session"));

        // Window should exist
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn restart_without_window_still_spawns() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Register agent without creating a window
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
                active: true,
                agent_definition: None,
            },
        );
        registry.save(&agents_dir, &feature).unwrap();

        let msg = agent_restart(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("Restarted agent 'reviewer'"));
    }

    #[test]
    fn restart_preserves_agent_definition_alias() {
        // After spawning an aliased agent (display name != definition),
        // a restart must preserve the stored definition so the new claude
        // process is launched with the same `--agent <def>` flag.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn::agent_spawn(
            dir.path(),
            &feature,
            "frontend-dev",
            Some("implementer"),
            None,
            false,
            server.name(),
        )
        .unwrap();

        let msg = agent_restart(dir.path(), &feature, "frontend-dev", server.name()).unwrap();
        assert!(msg.contains("Restarted agent 'frontend-dev'"));

        // Window still exists under display name
        assert!(
            tmux::find_window(server.name(), &session_name, "frontend-dev")
                .unwrap()
                .is_some()
        );

        // Registry retains the definition
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get("frontend-dev").unwrap();
        assert_eq!(entry.agent_definition.as_deref(), Some("implementer"));
    }

    #[test]
    fn restart_errors_for_unknown_agent() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_restart(dir.path(), &feature, "nonexistent", server.name());
        assert!(result.is_err());
    }

    #[test]
    fn restart_many_continues_on_error() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Register only reviewer
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
                active: true,
                agent_definition: None,
            },
        );
        registry.save(&agents_dir, &feature).unwrap();

        let results = agent_restart_many(
            dir.path(),
            &feature,
            &["reviewer".to_string(), "nonexistent".to_string()],
            server.name(),
        );
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
    }
}
