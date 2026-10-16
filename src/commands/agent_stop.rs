use std::path::Path;

use crate::error::Result;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Stop a running agent: end it in its window ([`tmux::panes::end_agent`])
/// and set `active = false` in the registry. Idempotent — succeeds even if
/// the window is already gone.
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
    let session_name = tmux::session_name(&config.project.name, feature);

    let mut registry = AgentRegistry::load(&agents_dir, feature)?;

    let entry = registry.require_mut(agent_name, feature)?;

    entry.active = false;
    registry.save(&agents_dir, feature)?;

    // Idempotent, and last: it may end this process.
    if let Some(target) = tmux::find_window(tmux_server, &session_name, agent_name)? {
        let _ = tmux::panes::end_agent(tmux_server, &target);
    }

    Ok(format!("Stopped agent '{agent_name}' in {feature}"))
}

/// Stop multiple agents. Continues on error, returns all results.
pub fn agent_stop_many(
    project_root: &Path,
    feature: &str,
    names: &[String],
    tmux_server: Option<&str>,
) -> Vec<Result<String>> {
    names
        .iter()
        .map(|name| agent_stop(project_root, feature, name, tmux_server))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_spawn::test_support::setup_project;
    use crate::testing::{TestServer, agent_entry};
    use tempfile::tempdir;

    fn register_agent(dir: &Path, feature: &str, name: &str) {
        let agents_dir = paths::agents_dir(dir);
        let mut registry = AgentRegistry::load(&agents_dir, feature).unwrap();
        registry.register(name, agent_entry(name));
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
        register_agent(dir.path(), &feature, "reviewer");

        let msg = agent_stop(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("Stopped agent 'reviewer'"));

        // Window should be gone
        let window = tmux::find_window(server.name(), &session_name, "reviewer").unwrap();
        assert!(window.is_none());

        // active flag should be false
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
        register_agent(dir.path(), &feature, "reviewer");

        let msg = agent_stop(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("Stopped agent 'reviewer'"));

        // active flag should be false
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

    #[test]
    fn stop_many_continues_on_error() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        register_agent(dir.path(), &feature, "reviewer");
        // "nonexistent" is not registered

        let results = agent_stop_many(
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
