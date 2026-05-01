use std::path::Path;

use crate::error::Result;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Delete an agent: remove its registry entry and inbox, then kill its
/// tmux window (if any). Unlike `agent stop`, this is terminal — the
/// entry is gone for good and the agent's queued messages, cursors, and
/// last-read metadata are wiped, so a future agent of the same name
/// starts from a clean slate.
///
/// On-disk state (registry + inbox) is fully consistent before the tmux
/// window is torn down, so we never leave a registry entry or inbox
/// referring to a window that has been killed.
pub fn agent_delete(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    tmux_server: Option<&str>,
) -> Result<String> {
    crate::messages::validate_name(agent_name, "agent")?;

    let pm_dir = paths::pm_dir(project_root);
    let agents_dir = paths::agents_dir(project_root);
    let messages_dir = paths::messages_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let session_name = tmux::session_name(&config.project.name, feature);

    let mut registry = AgentRegistry::load(&agents_dir, feature)?;

    // Verify agent exists in the registry
    if registry.get(agent_name).is_none() {
        return Err(crate::error::PmError::AgentNotFound(format!(
            "'{agent_name}' not found in scope '{feature}'"
        )));
    }

    registry.remove(agent_name);
    registry.save(&agents_dir, feature)?;

    // Wipe the agent's inbox so a future agent of the same name doesn't
    // inherit queued messages, cursors, or last-read metadata.
    crate::messages::delete_inbox(&messages_dir, feature, agent_name)?;

    // Kill the tmux window if it exists (idempotent, must be last so
    // that on-disk state is fully consistent first).
    if let Some(target) = tmux::find_window(tmux_server, &session_name, agent_name)? {
        let _ = tmux::kill_window(tmux_server, &target);
    }

    Ok(format!("Deleted agent '{agent_name}' from {feature}"))
}

/// Delete multiple agents. Continues on error, returns all results.
pub fn agent_delete_many(
    project_root: &Path,
    feature: &str,
    names: &[String],
    tmux_server: Option<&str>,
) -> Vec<Result<String>> {
    names
        .iter()
        .map(|name| agent_delete(project_root, feature, name, tmux_server))
        .collect()
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

    fn register_agent(dir: &Path, feature: &str, name: &str) {
        let agents_dir = paths::agents_dir(dir);
        let mut registry = AgentRegistry::load(&agents_dir, feature).unwrap();
        registry.register(
            name,
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: name.to_string(),
                active: true,
                agent_definition: None,
            },
        );
        registry.save(&agents_dir, feature).unwrap();
    }

    #[test]
    fn delete_kills_window_and_removes_entry() {
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

        let msg = agent_delete(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("Deleted agent 'reviewer'"));

        // Window should be gone
        let window = tmux::find_window(server.name(), &session_name, "reviewer").unwrap();
        assert!(window.is_none());

        // Registry entry should be gone
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("reviewer").is_none());
    }

    #[test]
    fn delete_idempotent_when_window_already_gone() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Register agent but don't create a window
        register_agent(dir.path(), &feature, "reviewer");

        let msg = agent_delete(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("Deleted agent 'reviewer'"));

        // Registry entry should be gone
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("reviewer").is_none());
    }

    #[test]
    fn delete_errors_for_unknown_agent() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_delete(dir.path(), &feature, "nonexistent", server.name());
        assert!(result.is_err());
    }

    #[test]
    fn delete_rejects_invalid_name() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_delete(dir.path(), &feature, "../evil", server.name());
        assert!(result.is_err());
    }

    #[test]
    fn delete_wipes_inbox() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        register_agent(dir.path(), &feature, "reviewer");

        // Send a message to the agent and persist last_read metadata
        let messages_dir = paths::messages_dir(dir.path());
        crate::messages::send(&messages_dir, &feature, "reviewer", "implementer", "hi").unwrap();
        let lr = crate::messages::LastRead {
            sender: "implementer".to_string(),
            sender_scope: None,
            sender_project: None,
            index: 0,
        };
        crate::messages::save_last_read(&messages_dir, &feature, "reviewer", &lr).unwrap();

        let inbox = messages_dir.join(&feature).join("reviewer");
        assert!(inbox.exists(), "inbox should exist before delete");

        agent_delete(dir.path(), &feature, "reviewer", server.name()).unwrap();

        assert!(!inbox.exists(), "inbox should be wiped after delete");
        // Feature messages directory itself is preserved
        assert!(messages_dir.join(&feature).exists());
    }

    #[test]
    fn delete_does_not_touch_other_agents_inboxes() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        register_agent(dir.path(), &feature, "reviewer");
        register_agent(dir.path(), &feature, "implementer");

        let messages_dir = paths::messages_dir(dir.path());
        crate::messages::send(&messages_dir, &feature, "reviewer", "user", "for-rev").unwrap();
        crate::messages::send(&messages_dir, &feature, "implementer", "user", "for-impl").unwrap();

        agent_delete(dir.path(), &feature, "reviewer", server.name()).unwrap();

        assert!(!messages_dir.join(&feature).join("reviewer").exists());
        assert!(messages_dir.join(&feature).join("implementer").exists());
    }

    #[test]
    fn delete_leaves_other_agents_intact() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        register_agent(dir.path(), &feature, "reviewer");
        register_agent(dir.path(), &feature, "implementer");

        agent_delete(dir.path(), &feature, "reviewer", server.name()).unwrap();

        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("reviewer").is_none());
        assert!(registry.get("implementer").is_some());
    }

    #[test]
    fn delete_many_continues_on_error() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        register_agent(dir.path(), &feature, "reviewer");
        // "nonexistent" is not registered

        let results = agent_delete_many(
            dir.path(),
            &feature,
            &["reviewer".to_string(), "nonexistent".to_string()],
            server.name(),
        );
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok());
        assert!(results[1].is_err());

        // reviewer should be gone, even though the second delete failed
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("reviewer").is_none());
    }
}
