use std::path::Path;

use crate::error::Result;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Stop a running agent: kill its tmux window and mark it inactive in the
/// registry. Idempotent — succeeds even if the window is already gone.
pub fn agent_stop(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    tmux_server: Option<&str>,
) -> Result<String> {
    crate::messages::validate_name(agent_name, "agent")?;

    let pm_dir = paths::pm_dir(project_root);
    let agents_dir = paths::agents_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let session_name = format!("{}/{feature}", config.project.name);

    let mut registry = AgentRegistry::load(&agents_dir, feature)?;

    // Verify agent exists in the registry
    if registry.get(agent_name).is_none() {
        return Err(crate::error::PmError::AgentNotFound(format!(
            "'{agent_name}' not found in scope '{feature}'"
        )));
    }

    // Kill the tmux window if it exists (idempotent)
    if let Some(target) = tmux::find_window(tmux_server, &session_name, agent_name)? {
        let _ = tmux::kill_window(tmux_server, &target);
    }

    // Mark inactive in the registry
    if let Some(entry) = registry.get_mut(agent_name) {
        entry.active = false;
    }
    registry.save(&agents_dir, feature)?;

    Ok(format!("Stopped agent '{agent_name}' in {feature}"))
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let session_name = format!("{project_name}/{feature_name}");
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        (session_name, feature_name.to_string())
    }

    fn register_agent(dir: &Path, feature: &str, name: &str, active: bool) {
        let agents_dir = paths::agents_dir(dir);
        let mut registry = AgentRegistry::load(&agents_dir, feature).unwrap();
        registry.register(
            name,
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: name.to_string(),
                active,
            },
        );
        registry.save(&agents_dir, feature).unwrap();
    }

    #[test]
    fn stop_kills_window_and_marks_inactive() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Create a window for the agent
        let worktree = dir.path().join(&feature);
        tmux::new_window(
            server.name(),
            &session_name,
            &worktree,
            Some("reviewer"),
            true,
        )
        .unwrap();
        register_agent(dir.path(), &feature, "reviewer", true);

        let msg = agent_stop(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("Stopped agent 'reviewer'"));

        // Window should be gone
        let window = tmux::find_window(server.name(), &session_name, "reviewer").unwrap();
        assert!(window.is_none());

        // Registry should show inactive
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(!registry.get("reviewer").unwrap().active);
    }

    #[test]
    fn stop_idempotent_when_window_already_gone() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Register agent but don't create a window
        register_agent(dir.path(), &feature, "reviewer", true);

        let msg = agent_stop(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("Stopped agent 'reviewer'"));

        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(!registry.get("reviewer").unwrap().active);
    }

    #[test]
    fn stop_errors_for_unknown_agent() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_stop(dir.path(), &feature, "nonexistent", server.name());
        assert!(result.is_err());
    }

    #[test]
    fn stop_rejects_invalid_name() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_stop(dir.path(), &feature, "../evil", server.name());
        assert!(result.is_err());
    }
}
