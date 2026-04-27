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
    fork_session: bool,
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
        // `--fork-session` only makes sense alongside `--resume`. It tells
        // Claude to load the source's transcript but assign a fresh
        // session id, so the fork's appends don't pollute the source.
        if fork_session {
            parts.push("--fork-session".to_string());
        }
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
    /// When `true` and `resume_session` is `Some`, passes `--fork-session`
    /// to Claude so the resumed conversation gets a fresh session id and
    /// the original is left untouched. Used by `pm agent fork`.
    pub fork_session: bool,
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
        params.fork_session,
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
                active: true,
            },
        );
        registry.save(&agents_dir, params.feature)?;
    }

    Ok(window_target)
}

/// Outcome of an [`agent_spawn`] call. Lets callers tell whether work was
/// actually done or the call was a no-op against an already-running agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnOutcome {
    /// Agent's window already existed; no spawn was performed.
    AlreadyActive,
    /// New tmux window was created (fresh agent or registry entry without
    /// a Claude session id).
    Spawned,
    /// Existing registry entry resumed via `claude --resume <id>`.
    Resumed,
}

impl SpawnOutcome {
    /// Returns true if a new window was created (Spawned or Resumed) rather
    /// than this being a no-op against an already-running agent.
    pub fn is_new_window(self) -> bool {
        match self {
            SpawnOutcome::Spawned | SpawnOutcome::Resumed => true,
            SpawnOutcome::AlreadyActive => false,
        }
    }
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
/// Returns a `(SpawnOutcome, status_message)` pair. The outcome distinguishes
/// no-op idempotent calls (`AlreadyActive`) from ones that actually created a
/// new tmux window (`Spawned`/`Resumed`) so callers like `agent_spawn_all`
/// can report accurate counts.
pub fn agent_spawn(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    context: Option<&str>,
    edit: bool,
    tmux_server: Option<&str>,
) -> Result<(SpawnOutcome, String)> {
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
                fork_session: false,
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

        // If the window still exists, the agent is already running.
        if let Some(target) = tmux::find_window(tmux_server, &session_name, agent_name)? {
            let msg = if context.is_some() {
                format!("Agent '{agent_name}' already active in {target} — sent context as message")
            } else {
                format!("Agent '{agent_name}' already active in {target}")
            };
            return Ok((SpawnOutcome::AlreadyActive, msg));
        }

        // Agent existed but window is gone — respawn.
        let window_target = spawn(None, resume_id.as_deref())?;

        let (outcome, msg) = if resume_id.is_some() {
            (
                SpawnOutcome::Resumed,
                format!("Resumed agent '{agent_name}' in {window_target}"),
            )
        } else {
            (
                SpawnOutcome::Spawned,
                format!("Spawned agent '{agent_name}' in {window_target}"),
            )
        };
        return Ok((outcome, msg));
    }

    // New agent, no positional prompt — the Stop hook blocks until any
    // queued context is available, then tells the agent to read it.
    let window_target = spawn(None, None)?;

    Ok((
        SpawnOutcome::Spawned,
        format!("Spawned agent '{agent_name}' in {window_target}"),
    ))
}

/// Result of `agent_spawn_all`, providing structured counts alongside messages.
pub struct SpawnAllResult {
    /// Human-readable success messages (one per registered agent processed
    /// without error — includes both newly-spawned and already-active agents).
    pub successes: Vec<String>,
    /// Human-readable error messages (one per failed agent).
    pub errors: Vec<String>,
    /// Number of agents that actually had a new tmux window created
    /// ([`SpawnOutcome::Spawned`] or [`SpawnOutcome::Resumed`]). Excludes
    /// already-active no-ops, so idempotent re-runs report `0`.
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
        .filter(|(_, e)| e.agent_type == AgentType::Agent && e.active)
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
    let mut spawned_count = 0;
    for name in &agent_names {
        match agent_spawn(project_root, feature, name, None, false, tmux_server) {
            Ok((outcome, msg)) => {
                if outcome.is_new_window() {
                    spawned_count += 1;
                }
                successes.push(msg);
            }
            Err(e) => errors.push(format!("Failed to spawn '{name}': {e}")),
        }
    }

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

        let (outcome, msg) =
            agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::Spawned);
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

        let (outcome, msg) =
            agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::AlreadyActive);
        assert!(!outcome.is_new_window());
        assert!(msg.contains("already active"));
    }

    #[test]
    fn spawn_existing_active_with_context_sends_message() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        setup_active_agent(&server, dir.path(), &session_name, &feature, "reviewer");

        let (outcome, msg) = agent_spawn(
            dir.path(),
            &feature,
            "reviewer",
            Some("focus on auth"),
            false,
            server.name(),
        )
        .unwrap();
        assert_eq!(outcome, SpawnOutcome::AlreadyActive);
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
    fn spawn_all_skips_inactive_agents() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn two agents
        agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        agent_spawn(dir.path(), &feature, "tester", None, false, server.name()).unwrap();

        // Mark tester as inactive (simulating `pm agent stop tester`)
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.get_mut("tester").unwrap().active = false;
        registry.save(&agents_dir, &feature).unwrap();

        // Kill the session and recreate it (simulating restart — windows gone)
        tmux::kill_session(server.name(), &session_name).unwrap();
        let worktree = dir.path().join("login");
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        // Respawn all — should only spawn reviewer
        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(result.spawned_count, 1);
        assert_eq!(result.successes.len(), 1);
        assert!(result.successes[0].contains("reviewer"));
        assert!(result.errors.is_empty());

        // reviewer window should exist, tester should not
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );
        assert!(
            tmux::find_window(server.name(), &session_name, "tester")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn spawn_all_idempotent_reports_zero_spawned() {
        // Regression for "pm open says Respawned N agents on every run".
        // When agents are already active, spawn_all should report
        // spawned_count == 0 even though every call succeeded.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        agent_spawn(dir.path(), &feature, "tester", None, false, server.name()).unwrap();

        // Call spawn_all without killing windows: every agent is already active.
        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();

        assert_eq!(
            result.spawned_count, 0,
            "no new windows should have been created; got spawned_count={}",
            result.spawned_count
        );
        // We still get success messages for every agent (they just say "already active").
        assert_eq!(result.successes.len(), 2);
        assert!(
            result
                .successes
                .iter()
                .all(|s| s.contains("already active"))
        );
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
                active: true,
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
        let (outcome, msg) =
            agent_spawn(dir.path(), &feature, "reviewer", None, false, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::Resumed);
        assert!(outcome.is_new_window());
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
        let cmd = build_claude_cmd(Some("reviewer"), None, None, None, false);
        assert_eq!(cmd, "claude --agent reviewer");
    }

    #[test]
    fn build_cmd_plain_session() {
        let cmd = build_claude_cmd(None, None, None, None, false);
        assert_eq!(cmd, "claude");
    }

    #[test]
    fn build_cmd_plain_session_with_permission() {
        let cmd = build_claude_cmd(None, None, None, Some("acceptEdits"), false);
        assert_eq!(cmd, "claude --permission-mode acceptEdits");
    }

    #[test]
    fn build_cmd_with_context() {
        let cmd = build_claude_cmd(
            Some("reviewer"),
            Some("review the auth module"),
            None,
            None,
            false,
        );
        assert_eq!(cmd, "claude --agent reviewer 'review the auth module'");
    }

    #[test]
    fn build_cmd_with_resume() {
        let cmd = build_claude_cmd(Some("reviewer"), None, Some("abc123"), None, false);
        assert_eq!(cmd, "claude --agent reviewer --resume abc123");
    }

    #[test]
    fn build_cmd_with_context_and_resume() {
        let cmd = build_claude_cmd(
            Some("reviewer"),
            Some("continue review"),
            Some("abc123"),
            None,
            false,
        );
        assert_eq!(
            cmd,
            "claude --agent reviewer --resume abc123 'continue review'"
        );
    }

    #[test]
    fn build_cmd_with_permission_mode() {
        let cmd = build_claude_cmd(Some("implementer"), None, None, Some("acceptEdits"), false);
        assert_eq!(
            cmd,
            "claude --agent implementer --permission-mode acceptEdits"
        );
    }

    #[test]
    fn build_cmd_with_fork_session() {
        // `--fork-session` only emits when paired with `--resume`.
        let cmd = build_claude_cmd(Some("reviewer"), None, Some("abc123"), None, true);
        assert_eq!(
            cmd,
            "claude --agent reviewer --resume abc123 --fork-session"
        );
    }

    #[test]
    fn build_cmd_fork_session_without_resume_is_noop() {
        // `--fork-session` requires `--resume` per claude's CLI; we drop it
        // silently rather than emit a broken command.
        let cmd = build_claude_cmd(Some("reviewer"), None, None, None, true);
        assert_eq!(cmd, "claude --agent reviewer");
    }
}
