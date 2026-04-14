use std::path::Path;

use crate::error::Result;
use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

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

/// Spawn a claude session in a tmux window. Works for both named agents
/// and plain claude sessions (when `agent_name` is None).
/// If `resume_session` is provided, passes `--resume` to claude.
/// Sets `PM_AGENT_NAME` in the spawned shell so the agent auto-identifies
/// in `pm msg send/check/read` without `--as-agent`.
///
/// When `reuse_window` is `Some(target)`, the existing window at that target
/// is renamed and reused instead of creating a new one. This is used during
/// `feat new --context` to reuse the default shell at window :0.
///
/// # Safety
/// Callers must validate `agent_name` via `validate_name()` before calling —
/// the name is interpolated into a shell command.
///
/// Returns the tmux window target.
#[allow(clippy::too_many_arguments)]
pub fn spawn_claude_session(
    project_root: &Path,
    feature: &str,
    agent_name: Option<&str>,
    prompt: Option<&str>,
    edit: bool,
    resume_session: Option<&str>,
    reuse_window: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<String> {
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let session_name = format!("{}/{feature}", config.project.name);
    let worktree_path = project_root.join(feature);

    // Resolve permission mode: --edit flag > project config > none
    let permission_mode = if edit {
        Some("acceptEdits".to_string())
    } else if let Some(name) = agent_name {
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
    let effective_prompt = match (prompt, agent_name) {
        (Some(p), _) => Some(p),
        (None, Some(_)) => Some("check messages"),
        (None, None) => None,
    };

    let window_name = agent_name.unwrap_or("claude");
    let cmd = build_claude_cmd(
        agent_name,
        effective_prompt,
        resume_session,
        permission_mode.as_deref(),
    );
    let window_target = if let Some(target) = reuse_window {
        tmux::rename_window(tmux_server, target, window_name)?;
        target.to_string()
    } else {
        tmux::new_window(
            tmux_server,
            &session_name,
            &worktree_path,
            Some(window_name),
            true,
        )?
    };

    // Set PM_AGENT_NAME so the agent's `pm msg send/check/read` calls
    // automatically identify as this agent without needing --as-agent.
    if let Some(name) = agent_name {
        let export_and_cmd = format!("export PM_AGENT_NAME={name} && {cmd}");
        tmux::send_keys(tmux_server, &window_target, &export_and_cmd)?;
    } else {
        tmux::send_keys(tmux_server, &window_target, &cmd)?;
    }

    // Register in agent registry if this is a named agent
    if let Some(name) = agent_name {
        let agents_dir = paths::agents_dir(project_root);
        let mut registry = AgentRegistry::load(&agents_dir, feature)?;
        registry.register(
            name,
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: name.to_string(),
                active: true,
            },
        );
        registry.save(&agents_dir, feature)?;
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
    let session_name = format!("{}/{feature}", config.project.name);

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
        let is_active = entry.active;
        let resume_id = if entry.session_id.is_empty() {
            None
        } else {
            Some(entry.session_id.clone())
        };

        if is_active
            && let Some(target) = tmux::find_window(tmux_server, &session_name, agent_name)?
        {
            if context.is_some() {
                return Ok(format!(
                    "Agent '{agent_name}' already active in {target} — sent context as message"
                ));
            }
            return Ok(format!("Agent '{agent_name}' already active in {target}"));
        }

        // Agent existed but window is gone — spawn with resume, no prompt.
        let window_target = spawn_claude_session(
            project_root,
            feature,
            Some(agent_name),
            None,
            edit,
            resume_id.as_deref(),
            None,
            tmux_server,
        )?;

        let msg = if resume_id.is_some() {
            format!("Resumed agent '{agent_name}' in {window_target}")
        } else {
            format!("Spawned agent '{agent_name}' in {window_target}")
        };
        return Ok(msg);
    }

    // New agent, no positional prompt — the Stop hook blocks until any
    // queued context is available, then tells the agent to read it.
    let window_target = spawn_claude_session(
        project_root,
        feature,
        Some(agent_name),
        None,
        edit,
        None,
        None,
        tmux_server,
    )?;

    Ok(format!("Spawned agent '{agent_name}' in {window_target}"))
}

/// Respawn all registered agents for a feature (excludes user-type entries).
/// Best-effort: tries every agent and collects errors rather than failing
/// on the first bad entry. Returns `(successes, errors)`.
pub fn agent_spawn_all(
    project_root: &Path,
    feature: &str,
    tmux_server: Option<&str>,
) -> Result<(Vec<String>, Vec<String>)> {
    let agents_dir = paths::agents_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;

    let agent_names: Vec<String> = registry
        .agents
        .iter()
        .filter(|(_, e)| e.agent_type == AgentType::Agent)
        .map(|(n, _)| n.clone())
        .collect();

    if agent_names.is_empty() {
        return Ok((vec!["No agents to respawn".to_string()], vec![]));
    }

    let mut successes = Vec::new();
    let mut errors = Vec::new();
    for name in &agent_names {
        match agent_spawn(project_root, feature, name, None, false, tmux_server) {
            Ok(msg) => successes.push(msg),
            Err(e) => errors.push(format!("Failed to spawn '{name}': {e}")),
        }
    }

    Ok((successes, errors))
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
        let session_name = format!("{project_name}/{feature_name}");
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
        assert!(entry.active);
        assert_eq!(entry.agent_type, AgentType::Agent);
    }

    #[test]
    fn spawn_existing_active_agent_returns_already_active() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        let msg =
            agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        assert!(msg.contains("already active"));
    }

    #[test]
    fn spawn_existing_active_with_context_sends_message() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
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

        // Mark them as inactive (simulating closed windows)
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        for entry in registry.agents.values_mut() {
            entry.active = false;
        }
        registry.save(&agents_dir, &feature).unwrap();

        // Kill the session and recreate it (simulating restart)
        tmux::kill_session(server.name(), &session_name).unwrap();
        let worktree = dir.path().join("login");
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        // Respawn all
        let (successes, errors) = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(successes.len(), 2);
        assert!(errors.is_empty());
    }

    #[test]
    fn spawn_all_no_agents_returns_message() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let (successes, errors) = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(successes, vec!["No agents to respawn"]);
        assert!(errors.is_empty());
    }

    #[test]
    fn spawn_all_partial_failure_continues() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Spawn a good agent first
        agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();

        // Manually register a second agent, then destroy the tmux session
        // so that spawning new windows fails for both. But first, mark
        // reviewer inactive and register a second agent.
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.register(
            "tester",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "tester".to_string(),
                active: false,
            },
        );
        registry.get_mut("reviewer").unwrap().active = false;
        registry.save(&agents_dir, &feature).unwrap();

        // Kill the session entirely — now both spawns will fail because
        // there's no tmux session to create windows in.
        let pm_dir = paths::pm_dir(dir.path());
        let config = ProjectConfig::load(&pm_dir).unwrap();
        let session_name = format!("{}/{feature}", config.project.name);
        tmux::kill_session(server.name(), &session_name).unwrap();

        let (successes, errors) = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();

        // Both should fail, but we get errors for both — not just the first
        assert!(successes.is_empty());
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("Failed to spawn"));
        assert!(errors[1].contains("Failed to spawn"));
    }

    #[test]
    fn spawn_resumes_dead_agent_with_session_id() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn agent, then manually set a session_id and mark inactive
        agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get_mut("reviewer").unwrap();
        entry.session_id = "sess-abc123".to_string();
        entry.active = false;
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

        // Verify it's marked active again
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("reviewer").unwrap().active);
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
}
