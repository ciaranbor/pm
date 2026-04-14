use std::path::Path;

use crate::error::Result;
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Check whether an agent definition file exists in any location that
/// `claude --agent <name>` would resolve when running in a feature worktree:
///   1. Feature worktree: `<project_root>/<feature>/.claude/agents/<name>.md`
///   2. Main worktree: `<project_root>/main/.claude/agents/<name>.md`
///      (where `pm agents install-project` writes; not committed to git but
///      still resolvable by Claude Code from sibling worktrees)
///   3. Global: `~/.claude/agents/<name>.md`
fn has_agent_definition(project_root: &Path, feature: &str, agent_name: &str) -> bool {
    let def_filename = format!("{agent_name}.md");

    // Feature worktree
    let feature_def = project_root
        .join(feature)
        .join(".claude/agents")
        .join(&def_filename);
    if feature_def.exists() {
        return true;
    }

    // Main worktree (project-level install location)
    let main_def = project_root
        .join("main")
        .join(".claude/agents")
        .join(&def_filename);
    if main_def.exists() {
        return true;
    }

    // Global (~/.claude/agents/)
    if let Some(home) = dirs::home_dir() {
        let global_def = home.join(".claude/agents").join(&def_filename);
        if global_def.exists() {
            return true;
        }
    }

    false
}

/// Check whether the recipient agent is currently active (registered and has a live tmux window).
fn is_agent_active(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    tmux_server: Option<&str>,
) -> Result<bool> {
    let agents_dir = paths::agents_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;

    if let Some(entry) = registry.get(agent_name)
        && entry.active
    {
        let pm_dir = paths::pm_dir(project_root);
        let config = ProjectConfig::load(&pm_dir)?;
        let session_name = format!("{}/{feature}", config.project.name);
        if tmux::find_window(tmux_server, &session_name, agent_name)?.is_some() {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Send a message to an agent's inbox. If the recipient agent is not active,
/// auto-spawns it first (equivalent to `pm agent spawn <name>`).
/// Returns status lines describing what happened.
pub fn agent_send(
    project_root: &Path,
    feature: &str,
    recipient: &str,
    sender: &str,
    body: &str,
    tmux_server: Option<&str>,
) -> Result<String> {
    // Deliver the message first, then spawn. This ensures the message is durably
    // queued in the inbox before the agent starts, so it will be picked up on first read.
    // If spawn fails, the message remains as a "dead letter" — acceptable since the user
    // can retry the spawn separately.
    let messages_dir = paths::messages_dir(project_root);
    let index = messages::send(&messages_dir, feature, recipient, sender, body)?;
    let mut status = format!("Message {index:03} sent to '{recipient}' (from '{sender}')");

    // Auto-spawn the agent if it's not currently active AND an agent
    // definition exists. Without a definition file spawning would create
    // a nonsensical agent, so we just deliver the message silently.
    if !is_agent_active(project_root, feature, recipient, tmux_server)?
        && has_agent_definition(project_root, feature, recipient)
    {
        let spawn_msg = super::agent_spawn::agent_spawn(
            project_root,
            feature,
            recipient,
            None,
            false,
            tmux_server,
        )?;
        status = format!("{status}\n{spawn_msg}");
    }

    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::agent::AgentRegistry;
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::state::project::{ProjectConfig, ProjectInfo};
    use crate::testing::TestServer;
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Set up a project with a tmux session for the feature.
    fn setup_project_with_tmux(dir: &Path, server: &TestServer) -> (PathBuf, String, String) {
        let root = dir.to_path_buf();
        let pm_dir = root.join(".pm");
        let project_name = server.scope("proj");
        let feature_name = "login";

        std::fs::create_dir_all(pm_dir.join("features")).unwrap();

        let config = ProjectConfig {
            project: ProjectInfo {
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

        (root, session_name, feature_name.to_string())
    }

    /// Create an agent definition in the main worktree (where `pm agents install-project` writes).
    fn create_agent_definition(root: &Path, agent_name: &str) {
        let agent_def = root
            .join("main")
            .join(".claude/agents")
            .join(format!("{agent_name}.md"));
        std::fs::create_dir_all(agent_def.parent().unwrap()).unwrap();
        std::fs::write(&agent_def, "# agent stub").unwrap();
    }

    #[test]
    fn send_to_active_agent_does_not_spawn() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        // Agent definition must exist so we're genuinely testing the
        // "active agent skips spawn" path, not the "no definition" guard.
        create_agent_definition(&root, "reviewer");

        // Spawn the agent first so it's active
        super::super::agent_spawn::agent_spawn(
            &root,
            &feature,
            "reviewer",
            None,
            false,
            server.name(),
        )
        .unwrap();

        let msg = agent_send(
            &root,
            &feature,
            "reviewer",
            "implementer",
            "hello",
            server.name(),
        )
        .unwrap();
        // Should just send the message, no spawn line
        assert_eq!(msg, "Message 001 sent to 'reviewer' (from 'implementer')");
        assert!(!msg.contains("Spawned"));
    }

    #[test]
    fn send_to_inactive_agent_auto_spawns() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        // Agent definition must exist for auto-spawn
        create_agent_definition(&root, "reviewer");

        // Send without spawning first — agent doesn't exist
        let msg = agent_send(
            &root,
            &feature,
            "reviewer",
            "implementer",
            "hello",
            server.name(),
        )
        .unwrap();
        assert!(msg.contains("Message 001 sent to 'reviewer'"));
        assert!(msg.contains("Spawned agent 'reviewer'"));

        // Verify the agent is now registered and active
        let agents_dir = paths::agents_dir(&root);
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get("reviewer").unwrap();
        assert!(entry.active);
    }

    #[test]
    fn send_to_dead_agent_auto_respawns() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        // Agent definition must exist for auto-spawn
        create_agent_definition(&root, "reviewer");

        // Spawn agent, then mark it inactive (simulating window died)
        super::super::agent_spawn::agent_spawn(
            &root,
            &feature,
            "reviewer",
            None,
            false,
            server.name(),
        )
        .unwrap();

        let agents_dir = paths::agents_dir(&root);
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.get_mut("reviewer").unwrap().active = false;
        registry.save(&agents_dir, &feature).unwrap();

        // Kill and recreate session to remove the window
        tmux::kill_session(server.name(), &session_name).unwrap();
        let worktree = root.join("login");
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        let msg = agent_send(
            &root,
            &feature,
            "reviewer",
            "implementer",
            "review this",
            server.name(),
        )
        .unwrap();
        assert!(msg.contains("Message 001 sent to 'reviewer'"));
        assert!(msg.contains("Spawned agent 'reviewer'"));
    }

    #[test]
    fn send_without_agent_definition_does_not_spawn() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        // No agent definition — send should deliver but not spawn
        let msg = agent_send(
            &root,
            &feature,
            "reviewer",
            "implementer",
            "hello",
            server.name(),
        )
        .unwrap();
        assert_eq!(msg, "Message 001 sent to 'reviewer' (from 'implementer')");
        assert!(!msg.contains("Spawned"));

        // Verify no agent was registered
        let agents_dir = paths::agents_dir(&root);
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("reviewer").is_none());
    }

    #[test]
    fn send_increments_index_in_output() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        // Agent definition must exist for auto-spawn on first send
        create_agent_definition(&root, "reviewer");

        // First send will auto-spawn
        agent_send(
            &root,
            &feature,
            "reviewer",
            "implementer",
            "first",
            server.name(),
        )
        .unwrap();
        let msg = agent_send(
            &root,
            &feature,
            "reviewer",
            "implementer",
            "second",
            server.name(),
        )
        .unwrap();
        assert!(msg.contains("Message 002"));
    }
}
