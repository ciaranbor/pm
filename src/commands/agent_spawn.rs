use std::path::Path;

use crate::error::Result;
use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Returns true if the process name is a common shell, indicating the agent
/// process has exited and the pane has fallen back to a shell prompt.
pub fn is_shell_process(cmd: &str) -> bool {
    matches!(
        cmd,
        "bash" | "zsh" | "sh" | "fish" | "dash" | "ksh" | "csh" | "tcsh"
    )
}

/// Build a claude command. If `agent_name` is provided, uses `--agent`.
/// Otherwise launches a plain claude session.
fn build_claude_cmd(
    agent_name: Option<&str>,
    prompt: Option<&str>,
    resume_session: Option<&str>,
    permission_mode: Option<&str>,
) -> String {
    let mut parts = vec!["claude".to_string()];

    if let Some(name) = agent_name {
        parts.push("--agent".to_string());
        parts.push(name.to_string());
    }

    if let Some(mode) = permission_mode {
        parts.push("--permission-mode".to_string());
        parts.push(mode.to_string());
    }

    if let Some(session_id) = resume_session {
        parts.push("--resume".to_string());
        parts.push(session_id.to_string());
    }

    if let Some(p) = prompt {
        parts.push(tmux::shell_quote(p));
    }

    parts.join(" ")
}

/// Parameters for spawning a Claude session in a tmux window.
pub struct SpawnClaudeParams<'a> {
    pub project_root: &'a Path,
    pub feature: &'a str,
    pub agent_name: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub edit: bool,
    pub resume_session: Option<&'a str>,
    /// When `Some(target)`, the existing window at that target is renamed and
    /// reused instead of creating a new one. Used during `feat new --context`
    /// to reuse the default shell at window :0.
    pub reuse_window: Option<&'a str>,
    pub tmux_server: Option<&'a str>,
}

/// Spawn a claude session in a tmux window. Works for both named agents
/// and plain claude sessions (when `agent_name` is None).
/// If `resume_session` is provided, passes `--resume` to claude.
/// Sets `PM_AGENT_NAME` in the spawned shell so the agent auto-identifies
/// in `pm msg send/check/read` without `--as-agent`.
///
/// # Safety
/// Callers must validate `agent_name` via `validate_name()` before calling —
/// the name is interpolated into a shell command.
///
/// Returns the tmux window target.
pub fn spawn_claude_session(params: &SpawnClaudeParams<'_>) -> Result<String> {
    let pm_dir = paths::pm_dir(params.project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    spawn_claude_session_with_config(params, &config)
}

/// Inner implementation that accepts a pre-loaded config to avoid redundant loads.
fn spawn_claude_session_with_config(
    params: &SpawnClaudeParams<'_>,
    config: &ProjectConfig,
) -> Result<String> {
    let session_name = tmux::session_name(&config.project.name, params.feature);
    let worktree_path = params.project_root.join(params.feature);

    // Resolve permission mode: --edit flag > project config > none
    let permission_mode = if params.edit {
        Some("acceptEdits".to_string())
    } else if let Some(name) = params.agent_name {
        config
            .agents
            .permissions
            .get(name)
            .filter(|s| !s.is_empty())
            .cloned()
    } else {
        None
    };

    // Named agents need a sentinel prompt when none is explicitly provided:
    // Claude with no positional prompt just waits for user input and never
    // completes a turn, so the Stop hook never fires. A trivial "continue"
    // prompt causes an immediate first turn, letting the blocking Stop hook
    // wait for messages. Plain (unnamed) claude sessions don't need this
    // since they're interactive by design.
    let effective_prompt = match (params.prompt, params.agent_name) {
        (Some(p), _) => Some(p),
        (None, Some(_)) => Some("Stand by."),
        (None, None) => None,
    };

    let window_name = params.agent_name.unwrap_or("claude");
    let cmd = build_claude_cmd(
        params.agent_name,
        effective_prompt,
        params.resume_session,
        permission_mode.as_deref(),
    );
    let window_target = if let Some(target) = params.reuse_window {
        tmux::rename_window(params.tmux_server, target, window_name)?;
        target.to_string()
    } else {
        tmux::new_window(
            params.tmux_server,
            &session_name,
            &worktree_path,
            Some(window_name),
            true,
        )?
    };

    // Set PM_AGENT_NAME so the agent's `pm msg send/check/read` calls
    // automatically identify as this agent without needing --as-agent.
    if let Some(name) = params.agent_name {
        let export_and_cmd = format!("export PM_AGENT_NAME={name} && {cmd}");
        tmux::send_keys(params.tmux_server, &window_target, &export_and_cmd)?;
    } else {
        tmux::send_keys(params.tmux_server, &window_target, &cmd)?;
    }

    // Register in agent registry if this is a named agent
    if let Some(name) = params.agent_name {
        let agents_dir = paths::agents_dir(params.project_root);
        let mut registry = AgentRegistry::load(&agents_dir, params.feature)?;
        registry.register(
            name,
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: name.to_string(),
            },
        );
        registry.save(&agents_dir, params.feature)?;
    }

    Ok(window_target)
}

/// Spawn a named agent in a tmux window within the feature session.
/// Handles three cases: new agent, already-active agent, and dead-but-resumable agent.
/// If `edit` is true, `--permission-mode acceptEdits` is passed.
/// Otherwise, the permission mode is looked up from the project config.
///
/// When `context` is provided, it is always enqueued as a message in the
/// agent's inbox rather than passed as a positional prompt. The Stop hook
/// blocks until the message is available, then tells the agent to read it.
/// The same path serves "spawn fresh with a brief", "spawn and nudge a
/// dead agent", and "send a follow-up to an active agent".
///
/// Returns a status message.
pub fn agent_spawn(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    context: Option<&str>,
    edit: bool,
    tmux_server: Option<&str>,
) -> Result<String> {
    crate::messages::validate_name(agent_name, "agent")?;

    let pm_dir = paths::pm_dir(project_root);
    let agents_dir = paths::agents_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let session_name = tmux::session_name(&config.project.name, feature);
    // Use _with_config helper to avoid reloading config in spawn_claude_session
    let spawn = |prompt: Option<&str>, resume: Option<&str>| {
        spawn_claude_session_with_config(
            &SpawnClaudeParams {
                project_root,
                feature,
                agent_name: Some(agent_name),
                prompt,
                edit,
                resume_session: resume,
                reuse_window: None,
                tmux_server,
            },
            &config,
        )
    };

    // Always queue context as a message before touching the session. That
    // way it survives a failed spawn as a dead letter and auto-arrives on
    // the empty first turn.
    if let Some(ctx) = context {
        let messages_dir = paths::messages_dir(project_root);
        let sender = crate::messages::default_user_name();
        crate::messages::send(&messages_dir, feature, agent_name, &sender, ctx)?;
    }

    let registry = AgentRegistry::load(&agents_dir, feature)?;

    // Check if this agent already exists in the registry
    if let Some(entry) = registry.get(agent_name) {
        let resume_id = if entry.session_id.is_empty() {
            None
        } else {
            Some(entry.session_id.clone())
        };

        // Check if the agent's tmux window still exists
        if let Some(target) = tmux::find_window(tmux_server, &session_name, agent_name)? {
            // Check if a claude process is actually running in the pane.
            // If the pane's current command is just a shell, the agent process
            // has exited and this is a zombie window — kill it and respawn.
            let is_zombie = match tmux::pane_command(tmux_server, &target) {
                Ok(cmd) => is_shell_process(&cmd),
                Err(_) => true, // pane query failed, treat as dead
            };

            if is_zombie {
                let _ = tmux::kill_window(tmux_server, &target);
            } else if context.is_some() {
                return Ok(format!(
                    "Agent '{agent_name}' already active in {target} — sent context as message"
                ));
            } else {
                return Ok(format!("Agent '{agent_name}' already active in {target}"));
            }
        }

        // Agent existed but window is gone — spawn with resume, no prompt.
        let window_target = spawn(None, resume_id.as_deref())?;

        let msg = if resume_id.is_some() {
            format!("Resumed agent '{agent_name}' in {window_target}")
        } else {
            format!("Spawned agent '{agent_name}' in {window_target}")
        };
        return Ok(msg);
    }

    // New agent, no positional prompt — the Stop hook blocks until any
    // queued context is available, then tells the agent to read it.
    let window_target = spawn(None, None)?;

    Ok(format!("Spawned agent '{agent_name}' in {window_target}"))
}

/// Result of `agent_spawn_all`, providing structured counts alongside messages.
pub struct SpawnAllResult {
    /// Human-readable success messages (one per spawned agent).
    pub successes: Vec<String>,
    /// Human-readable error messages (one per failed agent).
    pub errors: Vec<String>,
    /// Number of agents successfully spawned.
    pub spawned_count: usize,
}

/// Respawn all registered agents for a feature (excludes user-type entries).
/// Best-effort: tries every agent and collects errors rather than failing
/// on the first bad entry.
pub fn agent_spawn_all(
    project_root: &Path,
    feature: &str,
    tmux_server: Option<&str>,
) -> Result<SpawnAllResult> {
    let agents_dir = paths::agents_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;

    let agent_names: Vec<String> = registry
        .agents
        .iter()
        .filter(|(_, e)| e.agent_type == AgentType::Agent)
        .map(|(n, _)| n.clone())
        .collect();

    if agent_names.is_empty() {
        return Ok(SpawnAllResult {
            successes: vec!["No agents to respawn".to_string()],
            errors: vec![],
            spawned_count: 0,
        });
    }

    let mut successes = Vec::new();
    let mut errors = Vec::new();
    for name in &agent_names {
        match agent_spawn(project_root, feature, name, None, false, tmux_server) {
            Ok(msg) => successes.push(msg),
            Err(e) => errors.push(format!("Failed to spawn '{name}': {e}")),
        }
    }

    let spawned_count = successes.len();
    Ok(SpawnAllResult {
        successes,
        errors,
        spawned_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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

        // Write project config
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

        // Create feature state
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

        // Create worktree directory (simulated)
        let worktree = root.join(feature_name);
        std::fs::create_dir_all(&worktree).unwrap();

        // Create tmux session for the feature
        let session_name = tmux::session_name(&project_name, feature_name);
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        (session_name, feature_name.to_string())
    }

    #[test]
    fn spawn_creates_window_and_registers_agent() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        let msg =
            agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        assert!(msg.contains("Spawned agent 'reviewer'"));

        // Verify window was created
        let window = tmux::find_window(server.name(), &session_name, "reviewer").unwrap();
        assert!(window.is_some());

        // Verify agent is registered
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get("reviewer").unwrap();
        assert_eq!(entry.agent_type, AgentType::Agent);
    }

    /// Set up a fake "active" agent using the shared TestServer helper.
    fn setup_active_agent(
        server: &TestServer,
        dir: &Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        server.spawn_fake_agent(dir, session_name, feature, agent_name)
    }

    #[test]
    fn spawn_existing_active_agent_returns_already_active() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        setup_active_agent(&server, dir.path(), &session_name, &feature, "reviewer");

        let msg =
            agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        assert!(msg.contains("already active"));
    }

    #[test]
    fn spawn_zombie_window_respawns_agent() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Create a window named "reviewer" manually (simulating a zombie — window
        // exists but no claude process, just a shell).
        let worktree = dir.path().join("login");
        tmux::new_window(
            server.name(),
            &session_name,
            &worktree,
            Some("reviewer"),
            true,
        )
        .unwrap();

        // Register the agent as active in the registry
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.register(
            "reviewer",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
            },
        );
        registry.save(&agents_dir, &feature).unwrap();

        // The pane is running a shell (not claude), so agent_spawn should
        // detect the zombie, kill the stale window, and respawn.
        let msg =
            agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        assert!(
            msg.contains("Spawned") || msg.contains("Resumed"),
            "expected respawn message, got: {msg}"
        );

        // Verify the window still exists (the new one)
        let window = tmux::find_window(server.name(), &session_name, "reviewer").unwrap();
        assert!(window.is_some());
    }

    #[test]
    fn spawn_zombie_window_with_context_respawns_and_sends_message() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Create a zombie window
        let worktree = dir.path().join("login");
        tmux::new_window(
            server.name(),
            &session_name,
            &worktree,
            Some("reviewer"),
            true,
        )
        .unwrap();

        // Register as active
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.register(
            "reviewer",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
            },
        );
        registry.save(&agents_dir, &feature).unwrap();

        // Spawn with context — should detect zombie, respawn, and queue message
        let msg = agent_spawn(
            dir.path(),
            &feature,
            "reviewer",
            Some("review auth"),
            false,
            server.name(),
        )
        .unwrap();
        assert!(
            msg.contains("Spawned") || msg.contains("Resumed"),
            "expected respawn message, got: {msg}"
        );

        // Verify the message was queued
        let messages_dir = paths::messages_dir(dir.path());
        let summaries = crate::messages::check(&messages_dir, &feature, "reviewer").unwrap();
        assert_eq!(summaries.len(), 1);
    }

    #[test]
    fn spawn_existing_active_with_context_sends_message() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        setup_active_agent(&server, dir.path(), &session_name, &feature, "reviewer");

        let msg = agent_spawn(
            dir.path(),
            &feature,
            "reviewer",
            Some("focus on auth"),
            false,
            server.name(),
        )
        .unwrap();
        assert!(msg.contains("sent context as message"));

        // Verify the message was delivered
        let messages_dir = paths::messages_dir(dir.path());
        let summaries = crate::messages::check(&messages_dir, &feature, "reviewer").unwrap();
        assert_eq!(summaries.len(), 1);
    }

    #[test]
    fn spawn_all_respawns_agents() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn two agents
        agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        agent_spawn(dir.path(), &feature, "tester", None, false, server.name()).unwrap();

        // Kill the session and recreate it (simulating restart — windows gone)
        tmux::kill_session(server.name(), &session_name).unwrap();
        let worktree = dir.path().join("login");
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        // Respawn all
        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(result.spawned_count, 2);
        assert_eq!(result.successes.len(), 2);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn spawn_all_no_agents_returns_message() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(result.spawned_count, 0);
        assert_eq!(result.successes, vec!["No agents to respawn"]);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn spawn_all_partial_failure_continues() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Spawn a good agent first
        agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();

        // Manually register a second agent, then destroy the tmux session
        // so that spawning new windows fails for both.
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.register(
            "tester",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "tester".to_string(),
            },
        );
        registry.save(&agents_dir, &feature).unwrap();

        // Kill the session entirely — now both spawns will fail because
        // there's no tmux session to create windows in.
        let pm_dir = paths::pm_dir(dir.path());
        let config = ProjectConfig::load(&pm_dir).unwrap();
        let session_name = tmux::session_name(&config.project.name, &feature);
        tmux::kill_session(server.name(), &session_name).unwrap();

        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();

        // Both should fail, but we get errors for both — not just the first
        assert_eq!(result.spawned_count, 0);
        assert!(result.successes.is_empty());
        assert_eq!(result.errors.len(), 2);
        assert!(result.errors[0].contains("Failed to spawn"));
        assert!(result.errors[1].contains("Failed to spawn"));
    }

    #[test]
    fn spawn_resumes_dead_agent_with_session_id() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn agent, then manually set a session_id
        agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get_mut("reviewer").unwrap();
        entry.session_id = "sess-abc123".to_string();
        registry.save(&agents_dir, &feature).unwrap();

        // Kill the window (simulating it died)
        // Kill and recreate the session to clear the window
        tmux::kill_session(server.name(), &session_name).unwrap();
        let worktree = dir.path().join("login");
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        // Re-spawn should resume
        let msg =
            agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        assert!(msg.contains("Resumed agent 'reviewer'"));
    }

    #[test]
    fn spawn_rejects_invalid_name() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_spawn(dir.path(), &feature, "foo:bar", None, false, server.name());
        assert!(result.is_err());

        let result = agent_spawn(dir.path(), &feature, "../evil", None, false, server.name());
        assert!(result.is_err());
    }

    #[test]
    fn build_cmd_with_agent() {
        // Note: spawn_claude_session adds a " " sentinel when no prompt
        // is provided for a named agent. build_claude_cmd itself is
        // prompt-agnostic — the sentinel is injected by the caller.
        let cmd = build_claude_cmd(Some("reviewer"), None, None, None);
        assert_eq!(cmd, "claude --agent reviewer");
    }

    #[test]
    fn build_cmd_plain_session() {
        let cmd = build_claude_cmd(None, None, None, None);
        assert_eq!(cmd, "claude");
    }

    #[test]
    fn build_cmd_plain_session_with_permission() {
        let cmd = build_claude_cmd(None, None, None, Some("acceptEdits"));
        assert_eq!(cmd, "claude --permission-mode acceptEdits");
    }

    #[test]
    fn build_cmd_with_context() {
        let cmd = build_claude_cmd(Some("reviewer"), Some("review the auth module"), None, None);
        assert_eq!(cmd, "claude --agent reviewer 'review the auth module'");
    }

    #[test]
    fn build_cmd_with_resume() {
        let cmd = build_claude_cmd(Some("reviewer"), None, Some("abc123"), None);
        assert_eq!(cmd, "claude --agent reviewer --resume abc123");
    }

    #[test]
    fn build_cmd_with_context_and_resume() {
        let cmd = build_claude_cmd(
            Some("reviewer"),
            Some("continue review"),
            Some("abc123"),
            None,
        );
        assert_eq!(
            cmd,
            "claude --agent reviewer --resume abc123 'continue review'"
        );
    }

    #[test]
    fn build_cmd_with_permission_mode() {
        let cmd = build_claude_cmd(Some("implementer"), None, None, Some("acceptEdits"));
        assert_eq!(
            cmd,
            "claude --agent implementer --permission-mode acceptEdits"
        );
    }

    #[test]
    fn is_shell_process_detects_common_shells() {
        assert!(is_shell_process("bash"));
        assert!(is_shell_process("zsh"));
        assert!(is_shell_process("sh"));
        assert!(is_shell_process("fish"));
        assert!(!is_shell_process("claude"));
        assert!(!is_shell_process("node"));
        assert!(!is_shell_process("sleep"));
        assert!(!is_shell_process(""));
    }
}
