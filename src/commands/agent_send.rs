use std::path::Path;

use crate::error::{PmError, Result};
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectEntry;

/// Whether the recipient agent is currently flagged active. Loaded once
/// from disk per call to avoid redundant TOML parses.
///
/// `agent_send` no longer spawns *new* agents, so it doesn't need to know
/// whether a definition file or registry entry exists for resurrection —
/// only whether the agent is supposed to be running (`active`). A dead
/// window of an active agent is healed by `agent_spawn` after the message
/// is queued.
fn recipient_is_active(project_root: &Path, feature: &str, agent_name: &str) -> Result<bool> {
    let agents_dir = paths::agents_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;
    Ok(registry.get(agent_name).map(|e| e.active).unwrap_or(false))
}

/// Generate a helpful hint when a recipient agent is not found.
fn agent_not_found_hint(recipient: &str, sender_scope: &str, target_scope: &str) -> String {
    // If the name contains '@', the user may have used shorthand syntax
    // directly instead of letting dispatch parse it.
    if let Some(pos) = recipient.find('@') {
        let name = &recipient[..pos];
        let scope = &recipient[pos + 1..];
        if !name.is_empty() && !scope.is_empty() {
            return format!(
                "\n  Hint: Did you mean `pm msg send {name}@{scope}` (the @ shorthand is parsed by the CLI, not passed as the agent name)?"
            );
        }
    }

    // Common mistake: sending to "main" agent from a feature scope when they
    // meant to send to an agent in the main scope.
    if recipient == "main" && sender_scope != "main" {
        return "\n  Hint: 'main' is a scope, not an agent. To send to an agent in the main scope, \
             use `pm msg send <agent>@main` or `pm msg reply`"
            .to_string();
    }

    // If sender and target are in different scopes, remind about --project
    if sender_scope != target_scope {
        return format!(
            "\n  Hint: Agent '{recipient}' was not found in scope '{target_scope}'. \
             For cross-project messages, add `--project <name>`"
        );
    }

    // Default: suggest cross-scope syntax
    format!(
        "\n  Hint: The agent may exist in a different scope. \
         Try `pm msg send {recipient}@<scope>` or `--project <name>` for cross-project"
    )
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

/// Send a message to an agent's inbox.
///
/// `agent_send` is a near-pure queue: it never spawns a *new* agent. If the
/// recipient isn't registered or is flagged inactive (`active = false`), it
/// errors — delivering a message nobody can ever read is a mistake. If the
/// recipient is active but its tmux window has died, the message is queued
/// and the window is healed via `agent_spawn` (a no-op if the window is
/// alive).
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

    // The recipient must be an active agent. Messaging never conjures a new
    // agent — agents are stood up by `pm feat new`/`feat adopt` (the whole
    // team) or `pm agent spawn`.
    if !recipient_is_active(project_root, feature, recipient)? {
        let hint = agent_not_found_hint(recipient, sender_scope, feature);
        return Err(PmError::AgentNotFound(format!(
            "No active agent called '{recipient}' exists in scope '{feature}'.{hint}"
        )));
    }

    // Deliver the message first, then heal a dead window. This ensures the
    // message is durably queued in the inbox before the agent starts, so it
    // will be picked up on first read. If the heal spawn fails, the message
    // remains as a "dead letter" — acceptable since the user can retry.
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

    // The agent is active, but its tmux window may have died (crash,
    // accidental kill). Call `agent_spawn` to heal it: a no-op
    // (`AlreadyActive`) if the window is alive, a respawn/resume if it's
    // gone. Pass `None` for `agent_definition` so `agent_spawn` reads the
    // stored definition from the registry entry — preserving aliases. Only
    // append the spawn line when a heal actually happened, keeping the
    // common-case output byte-identical.
    let (outcome, spawn_msg) = super::agent_spawn::agent_spawn(
        project_root,
        feature,
        recipient,
        None,
        None,
        false,
        tmux_server,
    )?;
    if outcome.is_new_window() {
        status = format!("{status}\n{spawn_msg}");
    }

    Ok(status)
}

/// Parameters for sending a message to an agent in a different project.
pub struct CrossProjectSendParams<'a> {
    pub target_project_name: &'a str,
    pub sender_scope: &'a str,
    pub sender_project: &'a str,
    pub target_scope: &'a str,
    pub recipient: &'a str,
    pub sender: &'a str,
    pub body: &'a str,
}

/// Send a message to an agent in a different project. Looks up the target
/// project from the global registry, delivers the message to its
/// `.pm/messages/` directory, but does NOT auto-spawn the recipient
/// (we can't safely spawn agents in a foreign project).
pub fn agent_send_cross_project(params: &CrossProjectSendParams<'_>) -> Result<String> {
    let projects_dir = paths::global_projects_dir()?;
    agent_send_cross_project_with_dir(&projects_dir, params)
}

/// Inner implementation that accepts an explicit `projects_dir` for testability.
fn agent_send_cross_project_with_dir(
    projects_dir: &Path,
    params: &CrossProjectSendParams<'_>,
) -> Result<String> {
    let entry = ProjectEntry::load(projects_dir, params.target_project_name)?;
    let target_root = entry.root_path();

    let messages_dir = paths::messages_dir(&target_root);
    let index = messages::send_full(
        &messages_dir,
        params.target_scope,
        params.recipient,
        params.sender,
        params.body,
        Some(params.sender_scope),
        Some(params.sender_project),
    )?;

    Ok(format!(
        "Message {index:03} sent to '{recipient}@{target_scope}' in project '{target_project_name}' \
         (from '{sender}@{sender_scope}' in project '{sender_project}')",
        recipient = params.recipient,
        target_scope = params.target_scope,
        target_project_name = params.target_project_name,
        sender = params.sender,
        sender_scope = params.sender_scope,
        sender_project = params.sender_project,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::agent::AgentRegistry;
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::state::project::{ProjectConfig, ProjectInfo};
    use crate::testing::TestServer;
    use crate::tmux;
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
            workflow: None,
            created: now,
            last_active: now,
        };
        state.save(&pm_dir.join("features"), feature_name).unwrap();

        let worktree = root.join(feature_name);
        std::fs::create_dir_all(&worktree).unwrap();

        let session_name = tmux::session_name(&project_name, feature_name);
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        (root, session_name, feature_name.to_string())
    }

    /// Create an agent definition in the main worktree (where `pm agents install-project` writes).
    fn create_agent_definition(root: &Path, agent_name: &str) {
        let agent_def = paths::main_worktree(root)
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
            workflow: None,
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
            workflow: None,
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
            workflow: None,
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
        let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        // Agent definition must exist so we're genuinely testing the
        // "active agent skips spawn" path, not the "no definition" guard.
        create_agent_definition(&root, "reviewer");

        // Create a fake active agent (window running sleep, not a shell)
        server.spawn_fake_agent(&root, &session_name, &feature, "reviewer");

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
    fn send_to_unregistered_agent_errors_and_queues_nothing() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        // Even with a definition file present, an unregistered (never
        // spawned) agent is not a valid recipient — messaging never
        // conjures a new agent.
        create_agent_definition(&root, "reviewer");

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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No active agent called 'reviewer'")
        );

        // Nothing was queued.
        let messages_dir = paths::messages_dir(&root);
        assert!(
            messages::list(&messages_dir, &feature, "reviewer", None)
                .unwrap()
                .is_empty()
        );

        // Agent was not registered.
        let agents_dir = paths::agents_dir(&root);
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("reviewer").is_none());
    }

    #[test]
    fn send_to_active_dead_window_queues_and_respawns() {
        // An agent flagged active (active = true) whose tmux window has died
        // should have its message queued AND its window healed.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        create_agent_definition(&root, "reviewer");

        // Spawn agent (active = true), then kill its window (simulating crash).
        super::super::agent_spawn::agent_spawn(
            &root,
            &feature,
            "reviewer",
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        // Kill and recreate session to remove the window.
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
        // Message queued and the dead window healed.
        assert!(msg.contains("Message 001 sent to 'reviewer'"));
        assert!(msg.contains("Spawned agent 'reviewer'"));

        // The window now exists again.
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn send_to_stopped_agent_errors() {
        // `pm agent stop` flips active = false. A stopped agent is not a
        // valid recipient — messaging never resurrects a stopped agent.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        create_agent_definition(&root, "implementer");

        // Spawn aliased agent, then stop it (active = false).
        super::super::agent_spawn::agent_spawn(
            &root,
            &feature,
            "frontend-dev",
            Some("implementer"),
            None,
            false,
            server.name(),
        )
        .unwrap();
        super::super::agent_stop::agent_stop(&root, &feature, "frontend-dev", server.name())
            .unwrap();

        let result = agent_send(
            &root,
            &feature,
            None,
            "frontend-dev",
            "implementer",
            "hi",
            server.name(),
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No active agent called 'frontend-dev'")
        );

        // Nothing queued; agent stays inactive.
        let messages_dir = paths::messages_dir(&root);
        assert!(
            messages::list(&messages_dir, &feature, "frontend-dev", None)
                .unwrap()
                .is_empty()
        );
        let agents_dir = paths::agents_dir(&root);
        assert!(
            !AgentRegistry::load(&agents_dir, &feature)
                .unwrap()
                .get("frontend-dev")
                .unwrap()
                .active
        );
    }

    #[test]
    fn send_to_unspawned_agent_errors() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        // No agent registered at all — send should fail.
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
        assert!(err.contains("No active agent called 'reviewer' exists in scope"));
    }

    #[test]
    fn send_increments_index_in_output() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        // Pre-spawn the recipient active with a live window — messaging no
        // longer auto-spawns.
        create_agent_definition(&root, "reviewer");
        server.spawn_fake_agent(&root, &session_name, &feature, "reviewer");

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
        let main_worktree = paths::main_worktree(&root);
        std::fs::create_dir_all(&main_worktree).unwrap();
        let main_session = tmux::session_name(&config.project.name, "main");
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
            workflow: None,
            created: now,
            last_active: now,
        };
        main_state.save(&pm_dir.join("features"), "main").unwrap();

        // Agent definition in main worktree, and pre-spawn it active —
        // cross-scope sends require the target's agent to already be running.
        create_agent_definition(&root, "implementer");
        server.spawn_fake_agent(&root, &main_session, "main", "implementer");

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
        let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);

        create_agent_definition(&root, "reviewer");
        server.spawn_fake_agent(&root, &session_name, &feature, "reviewer");

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
        let main_worktree = paths::main_worktree(&root);
        std::fs::create_dir_all(&main_worktree).unwrap();
        let main_session = tmux::session_name(&config.project.name, "main");
        tmux::create_session(server.name(), &main_session, &main_worktree).unwrap();

        let now = Utc::now();
        let main_state = FeatureState {
            status: FeatureStatus::Wip,
            branch: "main".to_string(),
            worktree: "main".to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: now,
            last_active: now,
        };
        main_state.save(&pm_dir.join("features"), "main").unwrap();
        create_agent_definition(&root, "implementer");
        server.spawn_fake_agent(&root, &main_session, "main", "implementer");

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

    #[test]
    fn send_cross_scope_dead_window_queues_and_respawns() {
        // Cross-scope sends go through the same `agent_send` heal path as
        // same-scope: an active recipient in the target scope whose window
        // has died gets its message queued AND its window healed.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session_name, _feature) = setup_project_with_tmux(dir.path(), &server);

        // Set up "main" scope with tmux session + state.
        let pm_dir = root.join(".pm");
        let config = ProjectConfig::load(&pm_dir).unwrap();
        let main_worktree = paths::main_worktree(&root);
        std::fs::create_dir_all(&main_worktree).unwrap();
        let main_session = tmux::session_name(&config.project.name, "main");
        tmux::create_session(server.name(), &main_session, &main_worktree).unwrap();

        let now = Utc::now();
        let main_state = FeatureState {
            status: FeatureStatus::Wip,
            branch: "main".to_string(),
            worktree: "main".to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: now,
            last_active: now,
        };
        main_state.save(&pm_dir.join("features"), "main").unwrap();
        create_agent_definition(&root, "implementer");

        // Spawn the recipient active in main scope, then kill its window by
        // recreating the session (simulating a crash).
        super::super::agent_spawn::agent_spawn(
            &root,
            "main",
            "implementer",
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();
        tmux::kill_session(server.name(), &main_session).unwrap();
        tmux::create_session(server.name(), &main_session, &main_worktree).unwrap();

        // Cross-scope: login → main, recipient active but window dead.
        let msg = agent_send(
            &root,
            "login",
            Some("main"),
            "implementer",
            "reviewer",
            "cross-scope heal",
            server.name(),
        )
        .unwrap();

        // Queued and healed.
        assert!(msg.contains("implementer@main"));
        assert!(msg.contains("Spawned") || msg.contains("Resumed"));
        assert!(
            tmux::find_window(server.name(), &main_session, "implementer")
                .unwrap()
                .is_some()
        );
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
            repo_url: None,
            state_remote: None,
        };
        entry.save(projects_dir.path(), "exo").unwrap();

        let result = agent_send_cross_project_with_dir(
            projects_dir.path(),
            &CrossProjectSendParams {
                target_project_name: "exo",
                sender_scope: "login",
                sender_project: "myapp",
                target_scope: "main",
                recipient: "implementer",
                sender: "reviewer",
                body: "found a bug in the auth module",
            },
        )
        .unwrap();

        assert!(result.contains("Message 001"));
        assert!(result.contains("implementer@main"));
        assert!(result.contains("project 'exo'"));
        assert!(result.contains("reviewer@login"));
        assert!(result.contains("project 'myapp'"));

        // Verify message was actually delivered to the target project
        let messages_dir = paths::messages_dir(&target_root);
        let msg = messages::read_at(&messages_dir, "main", "implementer", "reviewer", 1)
            .unwrap()
            .unwrap();
        assert_eq!(msg.body, "found a bug in the auth module");
        assert_eq!(msg.meta.sender_scope.as_deref(), Some("login"));
        assert_eq!(msg.meta.sender_project.as_deref(), Some("myapp"));
    }

    #[test]
    fn cross_project_send_to_nonexistent_project_errors() {
        let projects_dir = tempdir().unwrap();
        std::fs::create_dir_all(projects_dir.path()).unwrap();

        let result = agent_send_cross_project_with_dir(
            projects_dir.path(),
            &CrossProjectSendParams {
                target_project_name: "nonexistent",
                sender_scope: "login",
                sender_project: "myapp",
                target_scope: "main",
                recipient: "implementer",
                sender: "reviewer",
                body: "hello",
            },
        );
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::ProjectNotFound(_)));
    }

    #[test]
    fn cross_project_send_records_sender_metadata() {
        let target_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();

        let target_root = setup_target_project(target_dir.path());

        let entry = ProjectEntry {
            root: target_root.to_str().unwrap().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(projects_dir.path(), "exo").unwrap();

        agent_send_cross_project_with_dir(
            projects_dir.path(),
            &CrossProjectSendParams {
                target_project_name: "exo",
                sender_scope: "my-feature",
                sender_project: "myapp",
                target_scope: "main",
                recipient: "bot",
                sender: "human",
                body: "test message",
            },
        )
        .unwrap();

        let messages_dir = paths::messages_dir(&target_root);
        let msg = messages::read_at(&messages_dir, "main", "bot", "human", 1)
            .unwrap()
            .unwrap();
        assert_eq!(msg.meta.sender_scope.as_deref(), Some("my-feature"));
        assert_eq!(msg.meta.sender_project.as_deref(), Some("myapp"));
    }

    #[test]
    fn cross_project_send_increments_index() {
        let target_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();

        let target_root = setup_target_project(target_dir.path());

        let entry = ProjectEntry {
            root: target_root.to_str().unwrap().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(projects_dir.path(), "exo").unwrap();

        let r1 = agent_send_cross_project_with_dir(
            projects_dir.path(),
            &CrossProjectSendParams {
                target_project_name: "exo",
                sender_scope: "feat",
                sender_project: "myapp",
                target_scope: "main",
                recipient: "bot",
                sender: "human",
                body: "first",
            },
        )
        .unwrap();
        assert!(r1.contains("Message 001"));

        let r2 = agent_send_cross_project_with_dir(
            projects_dir.path(),
            &CrossProjectSendParams {
                target_project_name: "exo",
                sender_scope: "feat",
                sender_project: "myapp",
                target_scope: "main",
                recipient: "bot",
                sender: "human",
                body: "second",
            },
        )
        .unwrap();
        assert!(r2.contains("Message 002"));
    }

    // --- agent_not_found_hint ---

    #[test]
    fn hint_main_as_recipient_from_feature() {
        let hint = agent_not_found_hint("main", "login", "login");
        assert!(hint.contains("'main' is a scope, not an agent"));
        assert!(hint.contains("pm msg send <agent>@main"));
    }

    #[test]
    fn hint_main_from_main_no_scope_suggestion() {
        // From main scope, "main" as recipient is just a missing agent, not a scope confusion
        let hint = agent_not_found_hint("main", "main", "main");
        assert!(!hint.contains("'main' is a scope"));
        assert!(hint.contains("different scope"));
    }

    #[test]
    fn hint_different_scope_suggests_project() {
        let hint = agent_not_found_hint("reviewer", "login", "other-feat");
        assert!(hint.contains("--project"));
    }

    #[test]
    fn hint_same_scope_default() {
        let hint = agent_not_found_hint("reviewer", "login", "login");
        assert!(hint.contains("different scope"));
        assert!(hint.contains("reviewer@<scope>"));
    }
}
