use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};
use crate::tmux;

/// Find the path to an agent definition file, checking locations that
/// `claude --agent <name>` would resolve when running in a feature worktree:
///   1. Feature worktree: `<project_root>/<feature>/.claude/agents/<name>.md`
///   2. Main worktree: `<project_root>/main/.claude/agents/<name>.md`
///      (where `pm agents install-project` writes; not committed to git but
///      still resolvable by Claude Code from sibling worktrees)
///   3. Global: `~/.claude/agents/<name>.md`
pub fn find_agent_definition_path(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
) -> Option<std::path::PathBuf> {
    let def_filename = format!("{agent_name}.md");

    // Feature worktree
    let feature_def = project_root
        .join(feature)
        .join(".claude/agents")
        .join(&def_filename);
    if feature_def.exists() {
        return Some(feature_def);
    }

    // Main worktree (project-level install location)
    let main_def = project_root
        .join("main")
        .join(".claude/agents")
        .join(&def_filename);
    if main_def.exists() {
        return Some(main_def);
    }

    // Global (~/.claude/agents/)
    if let Some(home) = dirs::home_dir() {
        let global_def = home.join(".claude/agents").join(&def_filename);
        if global_def.exists() {
            return Some(global_def);
        }
    }

    None
}

/// Check whether an agent definition file exists in any resolved location.
fn has_agent_definition(project_root: &Path, feature: &str, agent_name: &str) -> bool {
    find_agent_definition_path(project_root, feature, agent_name).is_some()
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

/// Resolve the `--upstream` flag to a concrete scope name by looking up
/// the current feature's base. Errors if the current scope is "main"
/// (no parent) or if the feature state cannot be loaded.
pub fn resolve_upstream(project_root: &Path, current_scope: &str) -> Result<String> {
    if current_scope == "main" {
        return Err(PmError::Messaging(
            "--upstream cannot be used from the main scope (there is no parent scope)".to_string(),
        ));
    }
    let features_dir = paths::features_dir(project_root);
    let state = FeatureState::load(&features_dir, current_scope)?;
    Ok(state.base_or_default().to_string())
}

/// Send a message to an agent's inbox. If the recipient agent is not active,
/// auto-spawns it first (equivalent to `pm agent spawn <name>`).
///
/// `target_scope` is the scope (feature or "main") the message is delivered
/// to. When `None`, defaults to `sender_scope` (same-scope message).
///
/// `sender_scope` is the scope the sender is currently in, recorded in
/// message metadata so the recipient knows where the message came from.
///
/// Returns status lines describing what happened.
pub fn agent_send(
    project_root: &Path,
    sender_scope: &str,
    target_scope: Option<&str>,
    recipient: &str,
    sender: &str,
    body: &str,
    tmux_server: Option<&str>,
) -> Result<String> {
    let feature = target_scope.unwrap_or(sender_scope);

    let is_active = is_agent_active(project_root, feature, recipient, tmux_server)?;

    // If the agent isn't running and no definition exists, fail early —
    // delivering a message that nobody can ever read is a mistake.
    if !is_active && !has_agent_definition(project_root, feature, recipient) {
        return Err(PmError::AgentNotFound(format!(
            "No agent called '{recipient}' exists in this scope. \
             Only agents with agent definitions are auto-spawned"
        )));
    }

    // Deliver the message first, then spawn. This ensures the message is durably
    // queued in the inbox before the agent starts, so it will be picked up on first read.
    // If spawn fails, the message remains as a "dead letter" — acceptable since the user
    // can retry the spawn separately.
    let messages_dir = paths::messages_dir(project_root);
    let is_cross_scope = target_scope.is_some() && target_scope != Some(sender_scope);
    let index = messages::send_with_scope(
        &messages_dir,
        feature,
        recipient,
        sender,
        body,
        if is_cross_scope {
            Some(sender_scope)
        } else {
            None
        },
    )?;
    let mut status = if is_cross_scope {
        format!(
            "Message {index:03} sent to '{recipient}@{feature}' (from '{sender}@{sender_scope}')"
        )
    } else {
        format!("Message {index:03} sent to '{recipient}' (from '{sender}')")
    };

    // Auto-spawn the agent if it's not currently active.
    if !is_active {
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

/// Send a message to an agent in a different project. Looks up the target
/// project from the global registry, delivers the message to its
/// `.pm/messages/` directory, but does NOT auto-spawn the recipient
/// (we can't safely spawn agents in a foreign project).
///
/// `sender_scope` is the scope the sender is in (used for metadata).
/// `target_scope` is the scope within the target project to deliver to.
pub fn agent_send_cross_project(
    target_project_name: &str,
    sender_scope: &str,
    target_scope: &str,
    recipient: &str,
    sender: &str,
    body: &str,
) -> Result<String> {
    let projects_dir = paths::global_projects_dir()?;
    agent_send_cross_project_with_dir(
        &projects_dir,
        target_project_name,
        sender_scope,
        target_scope,
        recipient,
        sender,
        body,
    )
}

/// Inner implementation that accepts an explicit `projects_dir` for testability.
fn agent_send_cross_project_with_dir(
    projects_dir: &Path,
    target_project_name: &str,
    sender_scope: &str,
    target_scope: &str,
    recipient: &str,
    sender: &str,
    body: &str,
) -> Result<String> {
    let entry = ProjectEntry::load(projects_dir, target_project_name)?;
    let target_root = PathBuf::from(&entry.root);

    let messages_dir = paths::messages_dir(&target_root);
    let index = messages::send_with_scope(
        &messages_dir,
        target_scope,
        recipient,
        sender,
        body,
        Some(sender_scope),
    )?;

    Ok(format!(
        "Message {index:03} sent to '{recipient}@{target_scope}' in project '{target_project_name}' \
         (from '{sender}@{sender_scope}')"
    ))
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

    // --- resolve_upstream ---

    fn setup_project_minimal(dir: &Path) -> PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        root
    }

    #[test]
    fn resolve_upstream_from_main_errors() {
        let dir = tempdir().unwrap();
        let root = setup_project_minimal(dir.path());

        let err = resolve_upstream(&root, "main").unwrap_err();
        assert!(format!("{err}").contains("--upstream cannot be used from the main scope"));
    }

    #[test]
    fn resolve_upstream_returns_base_branch() {
        let dir = tempdir().unwrap();
        let root = setup_project_minimal(dir.path());

        let now = Utc::now();
        let state = FeatureState {
            status: FeatureStatus::Wip,
            branch: "login".to_string(),
            worktree: "login".to_string(),
            base: "main".to_string(),
            pr: String::new(),
            context: String::new(),
            created: now,
            last_active: now,
        };
        state.save(&root.join(".pm/features"), "login").unwrap();

        assert_eq!(resolve_upstream(&root, "login").unwrap(), "main");
    }

    #[test]
    fn resolve_upstream_stacked_feature() {
        let dir = tempdir().unwrap();
        let root = setup_project_minimal(dir.path());

        let now = Utc::now();
        let state = FeatureState {
            status: FeatureStatus::Wip,
            branch: "login-v2".to_string(),
            worktree: "login-v2".to_string(),
            base: "login".to_string(),
            pr: String::new(),
            context: String::new(),
            created: now,
            last_active: now,
        };
        state.save(&root.join(".pm/features"), "login-v2").unwrap();

        assert_eq!(resolve_upstream(&root, "login-v2").unwrap(), "login");
    }

    #[test]
    fn resolve_upstream_defaults_to_main_when_base_empty() {
        let dir = tempdir().unwrap();
        let root = setup_project_minimal(dir.path());

        let now = Utc::now();
        let state = FeatureState {
            status: FeatureStatus::Wip,
            branch: "login".to_string(),
            worktree: "login".to_string(),
            base: String::new(), // empty base defaults to "main"
            pr: String::new(),
            context: String::new(),
            created: now,
            last_active: now,
        };
        state.save(&root.join(".pm/features"), "login").unwrap();

        assert_eq!(resolve_upstream(&root, "login").unwrap(), "main");
    }

    // --- agent_send ---

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
            None,
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
            None,
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
            None,
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
    fn send_without_agent_definition_errors() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        // No agent definition — send should fail
        let result = agent_send(
            &root,
            &feature,
            None,
            "reviewer",
            "implementer",
            "hello",
            server.name(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No agent called 'reviewer' exists in this scope"));
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
            None,
            "reviewer",
            "implementer",
            "first",
            server.name(),
        )
        .unwrap();
        let msg = agent_send(
            &root,
            &feature,
            None,
            "reviewer",
            "implementer",
            "second",
            server.name(),
        )
        .unwrap();
        assert!(msg.contains("Message 002"));
    }

    #[test]
    fn send_cross_scope_shows_scopes_in_output() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, _feature) = setup_project_with_tmux(dir.path(), &server);

        // Create a "main" scope setup with tmux session and agent definition
        let pm_dir = root.join(".pm");
        let config = ProjectConfig::load(&pm_dir).unwrap();
        let main_worktree = root.join("main");
        std::fs::create_dir_all(&main_worktree).unwrap();
        let main_session = format!("{}/main", config.project.name);
        tmux::create_session(server.name(), &main_session, &main_worktree).unwrap();

        // Need feature state for "main" scope so agent lookup works
        let now = Utc::now();
        let main_state = FeatureState {
            status: FeatureStatus::Wip,
            branch: "main".to_string(),
            worktree: "main".to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            created: now,
            last_active: now,
        };
        main_state.save(&pm_dir.join("features"), "main").unwrap();

        // Agent definition in main worktree
        create_agent_definition(&root, "implementer");

        // Send from "login" scope to "main" scope
        let msg = agent_send(
            &root,
            "login",
            Some("main"),
            "implementer",
            "reviewer",
            "please look at this",
            server.name(),
        )
        .unwrap();

        // Output should show cross-scope notation
        assert!(msg.contains("implementer@main"));
        assert!(msg.contains("reviewer@login"));
    }

    #[test]
    fn send_same_scope_does_not_record_sender_scope() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        create_agent_definition(&root, "reviewer");

        agent_send(
            &root,
            &feature,
            None,
            "reviewer",
            "implementer",
            "hello",
            server.name(),
        )
        .unwrap();

        let messages_dir = paths::messages_dir(&root);
        let msg = messages::read_at(&messages_dir, &feature, "reviewer", "implementer", 1)
            .unwrap()
            .unwrap();
        assert_eq!(msg.meta.sender_scope, None);
    }

    #[test]
    fn send_cross_scope_records_sender_scope_in_metadata() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, _feature) = setup_project_with_tmux(dir.path(), &server);

        // Set up "main" scope with tmux session
        let pm_dir = root.join(".pm");
        let config = ProjectConfig::load(&pm_dir).unwrap();
        let main_worktree = root.join("main");
        std::fs::create_dir_all(&main_worktree).unwrap();
        let main_session = format!("{}/main", config.project.name);
        tmux::create_session(server.name(), &main_session, &main_worktree).unwrap();

        let now = Utc::now();
        let main_state = FeatureState {
            status: FeatureStatus::Wip,
            branch: "main".to_string(),
            worktree: "main".to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            created: now,
            last_active: now,
        };
        main_state.save(&pm_dir.join("features"), "main").unwrap();
        create_agent_definition(&root, "implementer");

        // Cross-scope: login → main
        agent_send(
            &root,
            "login",
            Some("main"),
            "implementer",
            "reviewer",
            "cross-scope msg",
            server.name(),
        )
        .unwrap();

        let messages_dir = paths::messages_dir(&root);
        let msg = messages::read_at(&messages_dir, "main", "implementer", "reviewer", 1)
            .unwrap()
            .unwrap();
        assert_eq!(msg.meta.sender_scope.as_deref(), Some("login"));
    }

    // --- agent_send_cross_project ---

    /// Helper: set up a minimal project root with .pm dir.
    fn setup_target_project(dir: &Path) -> PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/messages")).unwrap();
        root
    }

    #[test]
    fn cross_project_send_delivers_message() {
        let target_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();

        let target_root = setup_target_project(target_dir.path());

        // Register target project in the projects directory
        let entry = ProjectEntry {
            root: target_root.to_str().unwrap().to_string(),
            main_branch: "main".to_string(),
        };
        entry.save(projects_dir.path(), "exo").unwrap();

        let result = agent_send_cross_project_with_dir(
            projects_dir.path(),
            "exo",
            "login",
            "main",
            "implementer",
            "reviewer",
            "found a bug in the auth module",
        )
        .unwrap();

        assert!(result.contains("Message 001"));
        assert!(result.contains("implementer@main"));
        assert!(result.contains("project 'exo'"));
        assert!(result.contains("reviewer@login"));

        // Verify message was actually delivered to the target project
        let messages_dir = paths::messages_dir(&target_root);
        let msg = messages::read_at(&messages_dir, "main", "implementer", "reviewer", 1)
            .unwrap()
            .unwrap();
        assert_eq!(msg.body, "found a bug in the auth module");
        assert_eq!(msg.meta.sender_scope.as_deref(), Some("login"));
    }

    #[test]
    fn cross_project_send_to_nonexistent_project_errors() {
        let projects_dir = tempdir().unwrap();
        std::fs::create_dir_all(projects_dir.path()).unwrap();

        let result = agent_send_cross_project_with_dir(
            projects_dir.path(),
            "nonexistent",
            "login",
            "main",
            "implementer",
            "reviewer",
            "hello",
        );
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::ProjectNotFound(_)));
    }

    #[test]
    fn cross_project_send_records_sender_scope() {
        let target_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();

        let target_root = setup_target_project(target_dir.path());

        let entry = ProjectEntry {
            root: target_root.to_str().unwrap().to_string(),
            main_branch: "main".to_string(),
        };
        entry.save(projects_dir.path(), "exo").unwrap();

        agent_send_cross_project_with_dir(
            projects_dir.path(),
            "exo",
            "my-feature",
            "main",
            "bot",
            "human",
            "test message",
        )
        .unwrap();

        let messages_dir = paths::messages_dir(&target_root);
        let msg = messages::read_at(&messages_dir, "main", "bot", "human", 1)
            .unwrap()
            .unwrap();
        // Cross-project always records sender_scope
        assert_eq!(msg.meta.sender_scope.as_deref(), Some("my-feature"));
    }

    #[test]
    fn cross_project_send_increments_index() {
        let target_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();

        let target_root = setup_target_project(target_dir.path());

        let entry = ProjectEntry {
            root: target_root.to_str().unwrap().to_string(),
            main_branch: "main".to_string(),
        };
        entry.save(projects_dir.path(), "exo").unwrap();

        let r1 = agent_send_cross_project_with_dir(
            projects_dir.path(),
            "exo",
            "feat",
            "main",
            "bot",
            "human",
            "first",
        )
        .unwrap();
        assert!(r1.contains("Message 001"));

        let r2 = agent_send_cross_project_with_dir(
            projects_dir.path(),
            "exo",
            "feat",
            "main",
            "bot",
            "human",
            "second",
        )
        .unwrap();
        assert!(r2.contains("Message 002"));
    }
}
