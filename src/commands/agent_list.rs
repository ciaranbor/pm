use std::path::Path;

use crate::error::Result;
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::paths;

/// List all agents for a feature from the agent registry.
pub fn agent_list(project_root: &Path, feature: &str, active_only: bool) -> Result<Vec<String>> {
    let agents_dir = paths::agents_dir(project_root);
    let messages_dir = paths::messages_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;

    if registry.agents.is_empty() {
        return Ok(vec!["No agents".to_string()]);
    }

    let mut lines = vec![format!("Agents in '{feature}':")];

    for (name, entry) in &registry.agents {
        if active_only && !entry.active {
            continue;
        }

        let status_str = if entry.active { "active" } else { "inactive" };
        // Surface aliases so users can see at a glance which display
        // names map to which claude agent definitions. Omitted when the
        // display name doubles as the definition (the common case).
        let alias_str = entry
            .agent_definition
            .as_deref()
            .filter(|def| *def != name)
            .map(|def| format!(", --agent {def}"))
            .unwrap_or_default();
        let summaries = messages::check(&messages_dir, feature, name)?;
        let unread: u32 = summaries.iter().map(|s| s.count).sum();
        let unread_str = if unread > 0 {
            format!(", {unread} unread")
        } else {
            String::new()
        };

        lines.push(format!("  {name} ({status_str}{alias_str}{unread_str})"));
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
    use tempfile::tempdir;

    fn setup_project(dir: &std::path::Path) {
        let root = dir.to_path_buf();
        let pm_dir = root.join(".pm");
        std::fs::create_dir_all(pm_dir.join("features")).unwrap();
    }

    #[test]
    fn list_no_agents_returns_no_agents() {
        let dir = tempdir().unwrap();
        setup_project(dir.path());

        let lines = agent_list(dir.path(), "login", false).unwrap();
        assert_eq!(lines, vec!["No agents"]);
    }

    #[test]
    fn list_from_registry_shows_status() {
        let dir = tempdir().unwrap();
        setup_project(dir.path());

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::default();
        // reviewer: active
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
        // tester: inactive
        registry.register(
            "tester",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "tester".to_string(),
                active: false,
                agent_definition: None,
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        let lines = agent_list(dir.path(), "login", false).unwrap();
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
    fn list_shows_alias_for_aliased_agents() {
        let dir = tempdir().unwrap();
        setup_project(dir.path());

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::default();
        // Aliased: frontend-dev -> implementer
        registry.register(
            "frontend-dev",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "frontend-dev".to_string(),
                active: true,
                agent_definition: Some("implementer".to_string()),
            },
        );
        // Non-aliased
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
        registry.save(&agents_dir, "login").unwrap();

        let lines = agent_list(dir.path(), "login", false).unwrap();
        let frontend_line = lines
            .iter()
            .find(|l| l.contains("frontend-dev"))
            .expect("frontend-dev line missing");
        assert!(
            frontend_line.contains("--agent implementer"),
            "expected '--agent implementer' segment, got: {frontend_line}"
        );

        let reviewer_line = lines
            .iter()
            .find(|l| l.contains("reviewer"))
            .expect("reviewer line missing");
        assert!(
            !reviewer_line.contains("--agent"),
            "non-aliased entries should not show --agent, got: {reviewer_line}"
        );
    }

    #[test]
    fn list_shows_unread_counts() {
        let dir = tempdir().unwrap();
        setup_project(dir.path());

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
        registry.save(&agents_dir, "login").unwrap();

        let mdir = paths::messages_dir(dir.path());
        messages::send(&mdir, "login", "reviewer", "implementer", "msg 1").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "msg 2").unwrap();

        let lines = agent_list(dir.path(), "login", false).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("reviewer") && l.contains("2 unread"))
        );
    }
}
