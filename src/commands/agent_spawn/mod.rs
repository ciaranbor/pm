//! `pm agent spawn`: spawning a named agent into its scope's tmux session,
//! or healing one whose window is gone or whose harness exited.
//!
//! With `--context`, on a (re)spawn the context is enqueued after the
//! definition validates, so a bad spawn leaves no message; an agent already
//! active gets it unvalidated. Unlike a team brief
//! ([`feat_common`](super::feat_common)) it is never refused on the
//! workflow's account, since it is also the path that heals a dead agent.

mod launch;
mod session;
mod spawn_all;
mod validate;

pub(crate) use launch::{LaunchConfig, configured_harness, definition_flag, resolve_launch};
pub use session::{
    RESUME_PROMPT, SPAWN_PROMPT, SpawnParams, SpawnedSession, is_launch_prompt, notes_suffix,
    spawn_session,
};
pub use spawn_all::{SpawnAllResult, agent_spawn_all};
pub(crate) use validate::validate_definition_resolves;

use std::path::Path;

use crate::commands::running_agents::{AgentAt, exited_pane};
use crate::error::Result;
use crate::harness;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig, resolve_harness_config};
use crate::tmux;
use session::spawn_session_with_config;

/// Outcome of an [`agent_spawn`] call. Lets callers tell whether work was
/// actually done or the call was a no-op against an already-running agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnOutcome {
    /// Agent's window already existed; no spawn was performed.
    AlreadyActive,
    /// New tmux window was created (fresh agent or registry entry without
    /// a session id).
    Spawned,
    /// Existing registry entry's session resumed.
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
/// An agent whose harness exited to the shell in its window is dead
/// ([`exited_pane`]) and starts again in that pane.
///
/// `agent_name` is the display name (registry key, tmux window, `PM_AGENT_NAME`).
/// `agent_definition` is the agent definition the harness launches. When
/// `None`:
///   - If a registry entry exists, its stored `agent_definition` is used (so
///     respawn / resume preserves the original definition).
///   - Otherwise, the display name doubles as the definition (back-compat).
///
/// When `Some(def)`, `def` is launched and stored on the registry entry for
/// future respawns.
///
/// When `context` is provided, it is always enqueued as a message in the
/// agent's inbox rather than passed as a positional prompt. The Stop hook
/// blocks until the message is available, then tells the agent to read it.
/// The same path serves "spawn fresh with a brief", "spawn and nudge a
/// dead agent", and "send a follow-up to an active agent".
///
/// Returns `(SpawnOutcome, status_message, notes)`. The outcome distinguishes
/// no-op idempotent calls (`AlreadyActive`) from ones that actually created a
/// new tmux window (`Spawned`/`Resumed`) so callers like `agent_spawn_all`
/// can report accurate counts. The notes carry every spawn-line remark —
/// dropped config rows, a harness change that skipped the stored session —
/// and the status message already includes them; they are returned
/// separately for callers that compose their own line.
pub fn agent_spawn(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    agent_definition: Option<&str>,
    context: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<(SpawnOutcome, String, Vec<String>)> {
    spawn_agent(
        project_root,
        feature,
        agent_name,
        agent_definition,
        context,
        None,
        tmux_server,
    )
}

/// [`agent_spawn`] into `pane`, a shell ready for the agent, whose window
/// takes its name, instead of a window of its own.
pub fn agent_spawn_in(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    pane: &str,
    tmux_server: Option<&str>,
) -> Result<(SpawnOutcome, String, Vec<String>)> {
    spawn_agent(
        project_root,
        feature,
        agent_name,
        None,
        None,
        Some(pane),
        tmux_server,
    )
}

fn spawn_agent(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    agent_definition: Option<&str>,
    context: Option<&str>,
    pane: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<(SpawnOutcome, String, Vec<String>)> {
    crate::messages::validate_name(agent_name, "agent")?;
    if let Some(def) = agent_definition {
        crate::messages::validate_name(def, "agent")?;
    }

    let pm_dir = paths::pm_dir(project_root);
    let agents_dir = paths::agents_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let global = GlobalConfig::load_or_default();
    let session_name = tmux::session_name(&config.project.name, feature);

    let registry = AgentRegistry::load(&agents_dir, feature)?;

    // Resolve the effective definition. Caller-provided override wins;
    // otherwise inherit any stored definition from a prior registration so
    // respawn/resume keeps using the definition the agent was originally
    // launched with. Falls back to None (which makes
    // spawn_session_with_config use `agent_name` as definition).
    let resolved_definition: Option<String> = agent_definition.map(String::from).or_else(|| {
        registry
            .get(agent_name)
            .and_then(|e| e.agent_definition.clone())
    });

    // The definition that will actually be launched (the resolved
    // definition, else the display name). Validation guards the spawn, not
    // message delivery — so it runs only on the (re)spawn paths below, never on
    // the already-active no-op.
    let effective_definition = resolved_definition.as_deref().unwrap_or(agent_name);

    // Queue context as a message. On spawn paths it's queued after validation
    // (so a bad def leaves no dead letter) but before the tmux spawn (so it
    // survives a later spawn failure as a dead letter, and pm's waiter wakes
    // the agent with it once its session starts).
    let queue_context = || -> Result<()> {
        if let Some(ctx) = context {
            let messages_dir = paths::messages_dir(project_root);
            let sender = crate::messages::default_user_name();
            crate::messages::send(&messages_dir, feature, agent_name, &sender, ctx)?;
        }
        Ok(())
    };

    // Use _with_config helper to avoid reloading config in spawn_session
    let spawn = |prompt: Option<&str>, resume: Option<&str>, reuse_window: Option<&str>| {
        spawn_session_with_config(
            &SpawnParams {
                project_root,
                feature,
                agent_name,
                agent_definition: resolved_definition.as_deref(),
                prompt,
                resume_session: resume,
                fork_session: false,
                reuse_window,
                tmux_server,
            },
            &config,
            &global,
        )
    };

    // Check if this agent already exists in the registry
    if let Some(entry) = registry.get(agent_name) {
        // Window still exists → agent is running, unless its harness exited
        // to the shell there. No respawn, so skip validation: a healthy agent
        // shouldn't go unreachable just because its def file moved since it
        // started. Context is still queued.
        let window = match pane {
            Some(_) => None,
            None => tmux::find_window(tmux_server, &session_name, agent_name)?,
        };
        // The pane whose harness exited, and the window it is in.
        let mut exited: Option<(String, String)> = None;
        if let Some(target) = window {
            let agent = AgentAt {
                project_root,
                scope: feature,
                name: agent_name,
                harness: entry.harness,
            };
            let harness_config = resolve_harness_config(&config.harness, &global.harness);
            match exited_pane(
                agent,
                &session_name,
                &entry.window_name,
                &harness_config,
                tmux_server,
            )? {
                Some(pane) => exited = Some((pane, target)),
                None => {
                    queue_context()?;
                    let msg = if context.is_some() {
                        format!(
                            "Agent '{agent_name}' already active in {target} — sent context as message"
                        )
                    } else {
                        format!("Agent '{agent_name}' already active in {target}")
                    };
                    return Ok((SpawnOutcome::AlreadyActive, msg, Vec::new()));
                }
            }
        }

        // Agent existed but its window is gone or its harness exited — respawn.
        validate_definition_resolves(project_root, effective_definition)?;
        let harness = configured_harness(effective_definition, &config.agents, &global.agents)?;
        let resume_id = harness::resumable_session(&entry.session_id, entry.harness, harness);
        queue_context()?;
        let reuse = match &exited {
            Some((exited_id, _)) => {
                tmux::panes::respawn(tmux_server, exited_id, &project_root.join(feature))?;
                Some(exited_id.as_str())
            }
            None => pane,
        };
        let SpawnedSession {
            window_target,
            mut notes,
            resumed,
        } = spawn(None, resume_id.as_deref(), reuse)?;
        // A pane respawned in reports the window it is in.
        let window_target = exited.map_or(window_target, |(_, exited_window)| exited_window);
        if entry.harness != harness && !entry.session_id.is_empty() {
            notes.insert(
                0,
                format!(
                    "harness changed {} → {harness}; previous session not resumed",
                    entry.harness
                ),
            );
        }

        let (outcome, mut msg) = if resumed {
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
        msg.push_str(&notes_suffix(&notes));
        return Ok((outcome, msg, notes));
    }

    // New agent, no positional prompt — the waiter wakes it for any queued
    // context.
    validate_definition_resolves(project_root, effective_definition)?;
    queue_context()?;
    let SpawnedSession {
        window_target,
        notes,
        ..
    } = spawn(None, None, pane)?;

    Ok((
        SpawnOutcome::Spawned,
        format!(
            "Spawned agent '{agent_name}' in {window_target}{}",
            notes_suffix(&notes)
        ),
        notes,
    ))
}

/// Helpers the submodules' tests share.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::harness::Harness;
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::state::paths;
    use crate::state::project::ProjectConfig;
    use crate::testing::TestServer;
    use crate::tmux;
    use chrono::Utc;
    use std::path::Path;

    /// Write stub `.agents/agents/<name>.md` files in the main worktree so
    /// `agent_spawn`'s pre-spawn definition check resolves in tests that
    /// build a project by hand (rather than through the global store).
    pub(crate) fn write_agent_defs(project_root: &Path, names: &[&str]) {
        let dir = paths::main_worktree(project_root).join(".agents/agents");
        std::fs::create_dir_all(&dir).unwrap();
        for name in names {
            std::fs::write(dir.join(format!("{name}.md")), "# stub").unwrap();
        }
    }

    pub(crate) fn setup_project(dir: &Path, server: &TestServer) -> (String, String) {
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
            agents: Default::default(),
            harness: Default::default(),
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
            workflow: None,
            created: now,
            last_active: now,
            team: Default::default(),
        };
        state.save(&pm_dir.join("features"), feature_name).unwrap();

        // Create worktree directory (simulated)
        let worktree = root.join(feature_name);
        std::fs::create_dir_all(&worktree).unwrap();

        // Install agent definition stubs so pre-spawn validation resolves.
        write_agent_defs(&root, &["reviewer", "tester", "implementer"]);

        // Create tmux session for the feature
        let session_name = tmux::session_name(&project_name, feature_name);
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        (session_name, feature_name.to_string())
    }

    /// Set up a fake "active" agent using the shared TestServer helper.
    pub(crate) fn setup_active_agent(
        server: &TestServer,
        dir: &Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        server.spawn_fake_agent(dir, session_name, feature, agent_name)
    }

    /// `definition` as `pm upgrade` projects it for opencode into `worktree`.
    pub(crate) fn project_opencode_definition(worktree: &Path, definition: &str) {
        let agents = worktree.join(Harness::OpenCode.config_dir()).join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join(format!("{definition}.md")), "# stub").unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_spawn::test_support::*;
    use crate::error::PmError;
    use crate::state::agent::{AgentRegistry, AgentType};
    use crate::state::runtime;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn spawn_creates_window_and_registers_agent() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        let left = runtime::Waiting::now(runtime::WaitingKind::Interrupted, None);
        runtime::write_waiting(dir.path(), &feature, "reviewer", &left).unwrap();

        let (outcome, msg, _) =
            agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::Spawned);
        assert_eq!(
            runtime::read_waiting(dir.path(), &feature, "reviewer").map(|w| w.kind),
            Some(runtime::WaitingKind::Startup),
            "an earlier spawn's marker is replaced"
        );
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

    #[test]
    fn spawn_existing_active_agent_returns_already_active() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        setup_active_agent(&server, dir.path(), &session_name, &feature, "reviewer");

        let (outcome, msg, _) =
            agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
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

        let (outcome, msg, _) = agent_spawn(
            dir.path(),
            &feature,
            "reviewer",
            None,
            Some("focus on auth"),
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
    fn spawn_resumes_dead_agent_with_session_id() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn agent, then manually set a session_id
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get_mut("reviewer").unwrap();
        entry.session_id = "sess-abc123".to_string();
        registry.save(&agents_dir, &feature).unwrap();

        // Kill and recreate the session to clear the window, as a crash or
        // reboot does, and respawn it.
        let worktree = dir.path().join("login");
        let respawn = || {
            tmux::kill_session(server.name(), &session_name).unwrap();
            tmux::create_session(server.name(), &session_name, &worktree).unwrap();
            let spawned =
                agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
            let window = format!("{session_name}:reviewer");
            server.wait_for_pane_text(&window, "--resume sess-abc123");
            (spawned, tmux::capture_pane(server.name(), &window).unwrap())
        };

        // With nothing unread it is told to carry on its work.
        let ((outcome, msg, _), pane) = respawn();
        assert_eq!(outcome, SpawnOutcome::Resumed);
        assert!(outcome.is_new_window());
        assert!(msg.contains("Resumed agent 'reviewer'"));
        assert!(pane.contains("'pm resumed this session"), "{pane}");
        assert!(!pane.contains(SPAWN_PROMPT), "{pane}");
        let prompted = || runtime::take_launch_prompt(dir.path(), &feature, "reviewer");
        assert!(prompted(), "its SessionStart leaves the waiter to the turn");

        // A message unread wakes it through the SessionStart waiter instead.
        crate::messages::send(
            &paths::messages_dir(dir.path()),
            &feature,
            "reviewer",
            "implementer",
            "look again",
        )
        .unwrap();
        let (_, pane) = respawn();
        assert!(!pane.contains("pm resumed this session"), "{pane}");
        assert!(!pane.contains(SPAWN_PROMPT), "{pane}");
        assert!(!prompted());
    }

    #[test]
    fn spawn_rejects_invalid_name() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_spawn(dir.path(), &feature, "foo:bar", None, None, server.name());
        assert!(result.is_err());

        let result = agent_spawn(dir.path(), &feature, "../evil", None, None, server.name());
        assert!(result.is_err());
    }

    #[test]
    fn spawn_with_alias_registers_under_display_name_with_definition() {
        // `pm agent spawn frontend-dev --agent implementer` should:
        //   - Register the entry under `frontend-dev`
        //   - Store `agent_definition = Some("implementer")` for restart/resume
        //   - Create a tmux window named `frontend-dev`
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        let (outcome, _msg, _) = agent_spawn(
            dir.path(),
            &feature,
            "frontend-dev",
            Some("implementer"),
            None,
            server.name(),
        )
        .unwrap();
        assert_eq!(outcome, SpawnOutcome::Spawned);

        // Window registered under display name
        assert!(
            tmux::find_window(server.name(), &session_name, "frontend-dev")
                .unwrap()
                .is_some()
        );
        // No window under definition name
        assert!(
            tmux::find_window(server.name(), &session_name, "implementer")
                .unwrap()
                .is_none()
        );

        // Registry entry under display name with stored definition
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get("frontend-dev").unwrap();
        assert_eq!(entry.agent_definition.as_deref(), Some("implementer"));
        assert_eq!(entry.window_name, "frontend-dev");
        assert_eq!(entry.effective_definition("frontend-dev"), "implementer");
        assert!(registry.get("implementer").is_none());
    }

    #[test]
    fn spawn_without_alias_stores_no_definition() {
        // The common case: `pm agent spawn implementer` should leave
        // `agent_definition` as `None`, so the on-disk TOML stays clean.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn(
            dir.path(),
            &feature,
            "implementer",
            None,
            None,
            server.name(),
        )
        .unwrap();

        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get("implementer").unwrap();
        assert!(entry.agent_definition.is_none());
        assert_eq!(entry.effective_definition("implementer"), "implementer");
    }

    #[test]
    fn spawn_alias_equal_to_name_does_not_persist_definition() {
        // `pm agent spawn implementer --agent implementer` is silly but
        // should be a no-op as far as on-disk state goes — keep TOML clean.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn(
            dir.path(),
            &feature,
            "implementer",
            Some("implementer"),
            None,
            server.name(),
        )
        .unwrap();

        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get("implementer").unwrap();
        assert!(entry.agent_definition.is_none());
    }

    #[test]
    fn spawn_rejects_invalid_definition_name() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Display name is fine, but the definition is invalid
        let result = agent_spawn(
            dir.path(),
            &feature,
            "frontend-dev",
            Some("foo:bar"),
            None,
            server.name(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn spawn_nonexistent_definition_errors_and_leaves_nothing() {
        // A typo'd agent name must fail loudly — and leave no registry entry
        // and no tmux window behind (the bug: success was reported over a
        // window whose `claude --agent` immediately errored).
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_spawn(
            dir.path(),
            &feature,
            "no-such-agent",
            None,
            None,
            server.name(),
        );
        assert!(matches!(
            result.unwrap_err(),
            PmError::AgentDefinitionMissing { .. }
        ));

        assert!(
            tmux::find_window(server.name(), &session_name, "no-such-agent")
                .unwrap()
                .is_none()
        );
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("no-such-agent").is_none());
    }

    #[test]
    fn spawn_with_context_for_missing_definition_queues_no_message() {
        // Validation runs before context is enqueued, so a bad spawn leaves
        // no dead-letter message in the inbox.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_spawn(
            dir.path(),
            &feature,
            "no-such-agent",
            None,
            Some("do the thing"),
            server.name(),
        );
        assert!(result.is_err());

        let messages_dir = paths::messages_dir(dir.path());
        let summaries = crate::messages::check(&messages_dir, &feature, "no-such-agent").unwrap();
        assert!(summaries.is_empty(), "no dead-letter should be queued");
    }

    #[test]
    fn context_reaches_active_agent_even_if_definition_removed() {
        // Regression: validation guards the *spawn*, not message delivery. An
        // already-running agent stays reachable even if its def file was
        // moved/removed since it started — the no-op path must not validate.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // A name no bundled definition claims, so removing the project's copy
        // leaves it unresolvable in either tier.
        write_agent_defs(dir.path(), &["sidekick"]);
        setup_active_agent(&server, dir.path(), &session_name, &feature, "sidekick");

        let def = paths::main_worktree(dir.path()).join(".agents/agents/sidekick.md");
        std::fs::remove_file(&def).unwrap();
        assert!(validate_definition_resolves(dir.path(), "sidekick").is_err());

        let (outcome, msg, _) = agent_spawn(
            dir.path(),
            &feature,
            "sidekick",
            None,
            Some("keep going"),
            server.name(),
        )
        .unwrap();
        assert_eq!(outcome, SpawnOutcome::AlreadyActive);
        assert!(msg.contains("sent context as message"));

        let messages_dir = paths::messages_dir(dir.path());
        let summaries = crate::messages::check(&messages_dir, &feature, "sidekick").unwrap();
        assert_eq!(summaries.len(), 1);
    }

    #[test]
    fn spawn_validates_effective_definition_not_display_name() {
        // `pm agent spawn frontend-dev --agent ghost-def`: the display name is
        // arbitrary, but the *definition* passed to `claude --agent` must
        // resolve. `ghost-def` doesn't, so this fails.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_spawn(
            dir.path(),
            &feature,
            "frontend-dev",
            Some("ghost-def"),
            None,
            server.name(),
        );
        assert!(matches!(
            result.unwrap_err(),
            PmError::AgentDefinitionMissing { .. }
        ));
        assert!(
            tmux::find_window(server.name(), &session_name, "frontend-dev")
                .unwrap()
                .is_none()
        );
    }
}
