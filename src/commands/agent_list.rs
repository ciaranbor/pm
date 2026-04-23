use std::path::Path;

use crate::error::Result;
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Returns true if the agent has a live (non-shell) process in its tmux window.
fn is_agent_active(
    tmux_server: Option<&str>,
    session_name: &str,
    window_name: &str,
) -> Result<bool> {
    if let Some(target) = tmux::find_window(tmux_server, session_name, window_name)? {
        match tmux::pane_command(tmux_server, &target) {
            Ok(cmd) => Ok(!super::agent_spawn::is_shell_process(&cmd)),
            Err(_) => Ok(false),
        }
    } else {
        Ok(false)
    }
}

/// List all agents for a feature from the agent registry.
pub fn agent_list(
    project_root: &Path,
    feature: &str,
    active_only: bool,
    tmux_server: Option<&str>,
) -> Result<Vec<String>> {
    let agents_dir = paths::agents_dir(project_root);
    let messages_dir = paths::messages_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let session_name = tmux::session_name(&config.project.name, feature);
    let registry = AgentRegistry::load(&agents_dir, feature)?;

    if registry.agents.is_empty() {
        return Ok(vec!["No agents".to_string()]);
    }

    let mut lines = vec![format!("Agents in '{feature}':")];

    for name in registry.agents.keys() {
        let active = is_agent_active(tmux_server, &session_name, name)?;

        if active_only && !active {
            continue;
        }

        let status_str = if active { "active" } else { "inactive" };
        let summaries = messages::check(&messages_dir, feature, name)?;
        let unread: u32 = summaries.iter().map(|s| s.count).sum();
        let unread_str = if unread > 0 {
            format!(", {unread} unread")
        } else {
            String::new()
        };

        lines.push(format!("  {name} ({status_str}{unread_str})"));
    }

    if lines.len() == 1 {
        let msg = if active_only {
            "No active agents"
        } else {
            "No agents"
        };
        return Ok(vec![msg.to_string()]);
    }

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages;
    use crate::state::agent::{AgentEntry, AgentType};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn setup_project(dir: &std::path::Path, server: &TestServer) -> String {
        let root = dir.to_path_buf();
        let pm_dir = root.join(".pm");
        let project_name = server.scope("proj");

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

        // Create worktree directory and tmux session
        let feature = "login";
        let worktree = root.join(feature);
        std::fs::create_dir_all(&worktree).unwrap();
        let session_name = tmux::session_name(&project_name, feature);
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        project_name
    }

    #[test]
    fn list_no_agents_returns_no_agents() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        setup_project(dir.path(), &server);

        let lines = agent_list(dir.path(), "login", false, server.name()).unwrap();
        assert_eq!(lines, vec!["No agents"]);
    }

    #[test]
    fn list_from_registry_shows_status() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let project_name = setup_project(dir.path(), &server);

        let session_name = tmux::session_name(&project_name, "login");

        // Spawn reviewer as active (non-shell process), tester registered but no window
        server.spawn_fake_agent(dir.path(), &session_name, "login", "reviewer");

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        registry.register(
            "tester",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "tester".to_string(),
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        let lines = agent_list(dir.path(), "login", false, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("reviewer") && l.contains("active"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("tester") && l.contains("inactive"))
        );
    }

    #[test]
    fn list_shows_unread_counts() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        setup_project(dir.path(), &server);

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        let mdir = paths::messages_dir(dir.path());
        messages::send(&mdir, "login", "reviewer", "implementer", "msg 1").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "msg 2").unwrap();

        let lines = agent_list(dir.path(), "login", false, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("reviewer") && l.contains("2 unread"))
        );
    }
}
