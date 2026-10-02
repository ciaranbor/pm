use std::path::Path;

use crate::error::{PmError, Result};
use crate::harness::{self, Harness, LaunchContext, SpawnSpec};
use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
use crate::state::paths;
use crate::state::project::{
    AgentSettings, AgentsConfig, GlobalConfig, HarnessConfig, ProjectConfig,
    resolve_agent_settings, resolve_harness_config,
};
use crate::state::runtime;
use crate::state::workflow;
use crate::tmux;

/// Pre-spawn check that the agent definition resolves to a real file, so a
/// typo'd or nonexistent `--agent <def>` fails loudly instead of printing
/// "Spawned …" over a tmux window whose harness errors out immediately — the
/// spawn is fire-and-forget, so that failure is invisible.
///
/// The `_with_home` split exists so resolution can be unit-tested against an
/// explicit home rather than the process's `$HOME`.
pub(crate) fn validate_definition_resolves(project_root: &Path, definition: &str) -> Result<()> {
    validate_definition_resolves_with_home(
        project_root,
        definition,
        paths::home_dir().ok().as_deref(),
    )
}

fn validate_definition_resolves_with_home(
    project_root: &Path,
    definition: &str,
    home: Option<&Path>,
) -> Result<()> {
    // The reserved vanilla name spawns with no definition — no file to check.
    if workflow::is_vanilla(definition) {
        return Ok(());
    }
    if workflow::definition_exists(project_root, definition, home) {
        return Ok(());
    }
    Err(PmError::AgentDefinitionMissing {
        agent: definition.to_string(),
        searched: workflow::definition_paths(project_root, definition, home),
    })
}

/// The agent definition a spawn keys on — for the harness's definition flag
/// and for the per-agent settings lookup alike. The explicit override wins;
/// otherwise the display name doubles as the definition (back-compat).
fn effective_definition<'a>(
    agent_definition: Option<&'a str>,
    agent_name: Option<&'a str>,
) -> Option<&'a str> {
    agent_definition.or(agent_name)
}

/// The settings a spawn actually launches with: config resolved for this
/// agent's definition. A `None` definition (a plain, unregistered session)
/// takes no config at all.
fn spawn_settings(
    definition: Option<&str>,
    project: &AgentsConfig,
    global: &AgentsConfig,
) -> Result<AgentSettings> {
    Ok(definition
        .map(|def| resolve_agent_settings(project, global, def))
        .transpose()?
        .unwrap_or_default())
}

/// The harness config selects for `definition`, checked before a respawn
/// or fork so a stored session id from a different harness isn't resumed.
pub(crate) fn configured_harness(
    definition: &str,
    project: &AgentsConfig,
    global: &AgentsConfig,
) -> Result<Harness> {
    Ok(resolve_agent_settings(project, global, definition)?.harness)
}

/// Directories outside the worktree an agent must be able to write, for a
/// harness that sandboxes: pm's state, the shared `.git` every worktree
/// writes through, the pm config dir, plus any `[harness.codex]
/// writable_roots` (relative ones resolve against the project root).
fn writable_dirs(project_root: &Path, config: &HarnessConfig) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![
        paths::pm_dir(project_root),
        paths::main_worktree(project_root).join(".git"),
    ];
    dirs.extend(paths::global_config_dir().ok());
    for root in config.codex.writable_roots.as_deref().unwrap_or(&[]) {
        let path = Path::new(root);
        dirs.push(if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        });
    }
    dirs
}

/// The shell line sent to the window: pm's own `PM_AGENT_NAME` (so `pm msg`
/// calls auto-identify) exported ahead of the harness command.
fn window_command(agent_name: Option<&str>, cmd: &str) -> String {
    match agent_name {
        Some(name) => format!("export PM_AGENT_NAME={name} && {cmd}"),
        None => cmd.to_string(),
    }
}

/// The definition that reaches the harness: the effective definition, except
/// the reserved vanilla name (any alias), which launches a definition-less
/// session even if a matching definition file happens to exist.
fn definition_flag(effective_definition: Option<&str>) -> Option<&str> {
    effective_definition.filter(|d| !workflow::is_vanilla(d))
}

/// The prompt a named agent is launched with when none is given, so its
/// first turn ends at once and the Stop hook takes over. It is not the
/// user's input (see [`super::hooks_user_prompt`]).
pub const SPAWN_PROMPT: &str = "Stand by.";

/// Parameters for spawning an agent session in a tmux window.
pub struct SpawnParams<'a> {
    pub project_root: &'a Path,
    pub feature: &'a str,
    /// Display name for the agent. Used as the tmux window name, the
    /// `PM_AGENT_NAME` env var (so `pm msg` calls auto-identify), and the
    /// registry key. `None` produces a plain session with no definition
    /// and no registry entry.
    pub agent_name: Option<&'a str>,
    /// Agent definition to launch. When `None` and `agent_name` is
    /// `Some(x)`, defaults to `x` (back-compat: display name doubles as
    /// definition name). When `Some(def)` with `agent_name = Some(name)`,
    /// you get a "named agent": registry key / window / `PM_AGENT_NAME` are
    /// all `name`, but the harness launches definition `def`. Ignored when
    /// `agent_name` is `None`.
    pub agent_definition: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub resume_session: Option<&'a str>,
    /// When `true` and `resume_session` is `Some`, the resumed conversation
    /// gets a fresh session id and the original is left untouched. Used by
    /// `pm agent fork`.
    pub fork_session: bool,
    /// When `Some(target)`, the shell at that target — a window's active
    /// pane, or a pane by id — runs the agent and its window is renamed,
    /// instead of a new window being made.
    pub reuse_window: Option<&'a str>,
    pub tmux_server: Option<&'a str>,
}

/// Spawn an agent session in a tmux window. Works for both named agents
/// and plain sessions (when `agent_name` is None). If `resume_session` is
/// provided, the harness resumes it. Sets `PM_AGENT_NAME` in the spawned
/// shell so the agent auto-identifies in `pm msg send/check/read` without
/// `--as-agent`.
///
/// # Safety
/// Callers must validate `agent_name` via `validate_name()` before calling —
/// the name is interpolated into a shell command.
///
pub fn spawn_session(params: &SpawnParams<'_>) -> Result<SpawnedSession> {
    let pm_dir = paths::pm_dir(params.project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    spawn_session_with_config(params, &config, &GlobalConfig::load_or_default())
}

/// What [`spawn_session`] launched: the tmux window target and the config
/// notes the caller should show alongside its own status line.
pub struct SpawnedSession {
    pub window_target: String,
    pub notes: Vec<String>,
    /// Whether the session asked for was the one opened. False for a fresh
    /// spawn, and when the harness had to replace the session it was given.
    pub resumed: bool,
}

/// The parenthetical a status line carries for config notes: empty when
/// there are none.
pub fn notes_suffix(notes: &[String]) -> String {
    if notes.is_empty() {
        String::new()
    } else {
        format!(" ({})", notes.join("; "))
    }
}

/// Inner implementation that accepts pre-loaded configs to avoid redundant
/// loads (`spawn_team` calls this once per agent).
fn spawn_session_with_config(
    params: &SpawnParams<'_>,
    config: &ProjectConfig,
    global: &GlobalConfig,
) -> Result<SpawnedSession> {
    let session_name = tmux::session_name(&config.project.name, params.feature);
    let worktree_path = params.project_root.join(params.feature);

    let effective_definition = effective_definition(params.agent_definition, params.agent_name);

    // Settings are configured per agent definition, not per display name,
    // and are re-resolved from config on every spawn — never stored on the
    // registry entry — so restart/fork/heal pick up config edits.
    let settings = spawn_settings(effective_definition, &config.agents, &global.agents)?;

    // Named agents need a sentinel prompt when none is explicitly provided:
    // a harness with no positional prompt just waits for user input and never
    // completes a turn, so the Stop hook never fires. A trivial "continue"
    // prompt causes an immediate first turn, letting the blocking Stop hook
    // wait for messages. Plain (unnamed) sessions don't need this since
    // they're interactive by design.
    let effective_prompt = match (params.prompt, params.agent_name) {
        (Some(p), _) => Some(p),
        (None, Some(_)) => Some(SPAWN_PROMPT),
        (None, None) => None,
    };

    let window_name = params.agent_name.unwrap_or(workflow::VANILLA_AGENT);

    // Compose the single `--append-system-prompt-file`: shared baseline plus
    // any non-empty notice boards. When no board has content this returns the
    // baseline path unchanged (or None when the baseline is also absent), so
    // older projects keep spawning exactly as before.
    let append_file =
        crate::notice::compose_spawn_prompt(params.project_root, params.feature, window_name)?;
    let harness_config = resolve_harness_config(&config.harness, &global.harness);
    let dirs = writable_dirs(params.project_root, &harness_config);
    let edit_dirs = if params.feature == "main" {
        Vec::new()
    } else {
        let summaries = paths::summaries_dir(params.project_root);
        std::fs::create_dir_all(&summaries)?;
        vec![summaries]
    };
    // A harness with a directory-trust gate would otherwise stop at an
    // interactive prompt nobody is watching.
    settings
        .harness
        .trust_worktree(&paths::home_dir()?, &worktree_path)?;
    let spec = SpawnSpec {
        definition: definition_flag(effective_definition),
        append_prompt_file: append_file.as_deref(),
        prompt: effective_prompt,
        resume_session: params.resume_session,
        fork_session: params.fork_session,
        permission_mode: settings.permission_mode.as_deref(),
        model: settings.model.as_deref(),
        writable_dirs: &dirs,
        edit_dirs: &edit_dirs,
    };
    let pre = settings
        .harness
        .pre_launch(
            &LaunchContext {
                project_root: params.project_root,
                feature: params.feature,
                worktree: &worktree_path,
                agent: window_name,
            },
            &spec,
            &harness_config,
        )
        // A refusal for want of a model row may be about one the
        // resolution dropped.
        .map_err(|e| match (e, &settings.model, &settings.dropped_model) {
            (PmError::Agent(message), None, Some(dropped)) => {
                PmError::Agent(format!("{message} ({dropped})"))
            }
            (e, _, _) => e,
        })?;
    let cmd = settings.harness.build_cmd(&spec, &harness_config, &pre);
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
    tmux::mark_agent_pane(params.tmux_server, &window_target)?;

    // Register before the command is sent: the harness's SessionStart hook
    // reads the entry, and a hook that fires first would find no agent —
    // on codex that means no role and no baseline, silently.
    if let Some(name) = params.agent_name {
        let agents_dir = paths::agents_dir(params.project_root);
        let mut registry = AgentRegistry::load(&agents_dir, params.feature)?;
        // Only persist `agent_definition` when it explicitly differs from
        // the registry key — keeps existing on-disk TOML clean for the
        // common case where display name == definition.
        let stored_definition = match params.agent_definition {
            Some(def) if def != name => Some(def.to_string()),
            _ => None,
        };
        registry.register(
            name,
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: pre.session_id.clone().unwrap_or_default(),
                window_name: name.to_string(),
                active: true,
                agent_definition: stored_definition,
                harness: settings.harness,
                spawned_at: Some(chrono::Utc::now()),
            },
        );
        registry.save(&agents_dir, params.feature)?;
        // Replaces whatever an earlier spawn left; the session's start
        // clears it, so one that outlives the start grace is a dialog
        // before the session (README, "Attention view").
        let startup = runtime::Waiting::now(runtime::WaitingKind::Startup, None);
        runtime::write_waiting(params.project_root, params.feature, name, &startup)?;
        runtime::clear_transcript(params.project_root, params.feature, name)?;
    }

    tmux::send_keys(
        params.tmux_server,
        &window_target,
        &window_command(params.agent_name, &cmd),
    )?;

    let resumed = match (params.resume_session, &pre.session_id) {
        (Some(_), _) if params.fork_session => false,
        (Some(asked), Some(opened)) => asked == opened,
        (asked, None) => asked.is_some(),
        (None, Some(_)) => false,
    };
    let mut notes = settings.notes;
    notes.extend(pre.notes);
    Ok(SpawnedSession {
        window_target,
        notes,
        resumed,
    })
}

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
    // survives a later spawn failure as a dead letter and auto-arrives on the
    // empty first turn).
    let queue_context = || -> Result<()> {
        if let Some(ctx) = context {
            let messages_dir = paths::messages_dir(project_root);
            let sender = crate::messages::default_user_name();
            crate::messages::send(&messages_dir, feature, agent_name, &sender, ctx)?;
        }
        Ok(())
    };

    // Use _with_config helper to avoid reloading config in spawn_session
    let spawn = |prompt: Option<&str>, resume: Option<&str>| {
        spawn_session_with_config(
            &SpawnParams {
                project_root,
                feature,
                agent_name: Some(agent_name),
                agent_definition: resolved_definition.as_deref(),
                prompt,
                resume_session: resume,
                fork_session: false,
                reuse_window: pane,
                tmux_server,
            },
            &config,
            &global,
        )
    };

    // Check if this agent already exists in the registry
    if let Some(entry) = registry.get(agent_name) {
        // Window still exists → agent is running. No respawn, so skip
        // validation: a healthy agent shouldn't go unreachable just because its
        // def file moved since it started. Context is still queued.
        let window = match pane {
            Some(_) => None,
            None => tmux::find_window(tmux_server, &session_name, agent_name)?,
        };
        if let Some(target) = window {
            queue_context()?;
            let msg = if context.is_some() {
                format!("Agent '{agent_name}' already active in {target} — sent context as message")
            } else {
                format!("Agent '{agent_name}' already active in {target}")
            };
            return Ok((SpawnOutcome::AlreadyActive, msg, Vec::new()));
        }

        // Agent existed but window is gone — respawn.
        validate_definition_resolves(project_root, effective_definition)?;
        let harness = configured_harness(effective_definition, &config.agents, &global.agents)?;
        let resume_id = harness::resumable_session(&entry.session_id, entry.harness, harness);
        queue_context()?;
        let SpawnedSession {
            window_target,
            mut notes,
            resumed,
        } = spawn(None, resume_id.as_deref())?;
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

    // New agent, no positional prompt — the Stop hook blocks until any queued
    // context is available, then tells the agent to read it.
    validate_definition_resolves(project_root, effective_definition)?;
    queue_context()?;
    let SpawnedSession {
        window_target,
        notes,
        ..
    } = spawn(None, None)?;

    Ok((
        SpawnOutcome::Spawned,
        format!(
            "Spawned agent '{agent_name}' in {window_target}{}",
            notes_suffix(&notes)
        ),
        notes,
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
    // `agent_spawn` reads the stored `agent_definition` from the registry
    // when called with `None`, so respawns automatically preserve aliases.
    for name in &agent_names {
        match agent_spawn(project_root, feature, name, None, None, tmux_server) {
            Ok((outcome, msg, _)) => {
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
pub(crate) mod tests {
    use super::*;
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::testing::TestServer;
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Write stub `.agents/agents/<name>.md` files in the main worktree so
    /// `agent_spawn`'s pre-spawn definition check resolves in tests that
    /// build a project by hand (rather than through the global store).
    fn write_agent_defs(project_root: &Path, names: &[&str]) {
        let dir = paths::main_worktree(project_root).join(".agents/agents");
        std::fs::create_dir_all(&dir).unwrap();
        for name in names {
            std::fs::write(dir.join(format!("{name}.md")), "# stub").unwrap();
        }
    }

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
            progress: Default::default(),
            blocked_reason: None,
            blocked_by: None,
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
    fn spawn_all_respawns_agents() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn two agents
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        agent_spawn(dir.path(), &feature, "tester", None, None, server.name()).unwrap();

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
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        agent_spawn(dir.path(), &feature, "tester", None, None, server.name()).unwrap();

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

        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        agent_spawn(dir.path(), &feature, "tester", None, None, server.name()).unwrap();

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
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();

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
                agent_definition: None,
                harness: Harness::ClaudeCode,
                spawned_at: None,
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
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();

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
        let (outcome, msg, _) =
            agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::Resumed);
        assert!(outcome.is_new_window());
        assert!(msg.contains("Resumed agent 'reviewer'"));
    }

    /// Point `definition`'s `[agents.harness]` row at `harness` in the
    /// project config.
    fn configure_harness(project_root: &Path, definition: &str, harness: &str) {
        let pm_dir = paths::pm_dir(project_root);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert(definition.to_string(), harness.to_string());
        config.save(&pm_dir).unwrap();
    }

    #[test]
    fn spawn_on_codex_launches_codex_trusts_the_worktree_and_records_the_harness() {
        let _guard = crate::testing::CODEX_CONFIG_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        configure_harness(dir.path(), "reviewer", "codex");
        let worktree = dir.path().join(&feature);
        assert!(!Harness::Codex.worktree_trusted(&paths::home_dir().unwrap(), &worktree));

        let (outcome, _, _) =
            agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::Spawned);

        let target = tmux::find_window(server.name(), &session_name, "reviewer")
            .unwrap()
            .expect("window");
        server.wait_for_pane_text(
            &target,
            "PM_AGENT_NAME=reviewer && codex --no-daemon -a 'never' -s 'danger-full-access' 'Stand by.'",
        );
        let registry = AgentRegistry::load(&paths::agents_dir(dir.path()), &feature).unwrap();
        assert_eq!(registry.get("reviewer").unwrap().harness, Harness::Codex);
        assert!(Harness::Codex.worktree_trusted(&paths::home_dir().unwrap(), &worktree));
    }

    #[test]
    fn spawn_drops_global_row_bound_to_another_harness_and_reports_it() {
        // Through the chokepoint with an explicit global config, so the
        // shared test home is left untouched.
        let _guard = crate::testing::CODEX_CONFIG_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        configure_harness(dir.path(), "reviewer", "codex");
        let config = ProjectConfig::load(&paths::pm_dir(dir.path())).unwrap();
        let mut global = GlobalConfig::default();
        global
            .agents
            .models
            .insert("reviewer".to_string(), "opus".to_string());

        let spawned = spawn_session_with_config(
            &SpawnParams {
                project_root: dir.path(),
                feature: &feature,
                agent_name: Some("reviewer"),
                agent_definition: None,
                prompt: None,
                resume_session: None,
                fork_session: false,
                reuse_window: None,
                tmux_server: server.name(),
            },
            &config,
            &global,
        )
        .unwrap();
        assert_eq!(
            spawned.notes,
            vec![
                "global [agents.models] row for 'reviewer' is bound to claude-code, not codex \
                 — not applied"
            ]
        );
        let target = tmux::find_window(server.name(), &session_name, "reviewer")
            .unwrap()
            .unwrap();
        server.wait_for_pane_text(
            &target,
            "&& codex --no-daemon -a 'never' -s 'danger-full-access' 'Stand by.'",
        );
        let text = tmux::capture_pane(server.name(), &target).unwrap();
        assert!(!text.contains("-m "), "{text}");
    }

    #[test]
    fn respawn_after_harness_change_starts_fresh_and_says_so() {
        // A session id only means something to the harness that produced
        // it: once config moves the definition to another harness, the dead
        // agent is respawned without `resume`, and the entry follows.
        let _guard = crate::testing::CODEX_CONFIG_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.get_mut("reviewer").unwrap().session_id = "cc-session".to_string();
        registry.save(&agents_dir, &feature).unwrap();

        tmux::kill_session(server.name(), &session_name).unwrap();
        tmux::create_session(server.name(), &session_name, &dir.path().join(&feature)).unwrap();
        configure_harness(dir.path(), "reviewer", "codex");
        // A `"*"` model row binds to the default harness, so it is dropped
        // for the codex agent: both remarks share one parenthetical.
        let pm_dir = paths::pm_dir(dir.path());
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .models
            .insert("*".to_string(), "opus".to_string());
        config.save(&pm_dir).unwrap();

        let (outcome, msg, notes) =
            agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::Spawned);
        assert_eq!(
            notes,
            vec![
                "harness changed claude-code → codex; previous session not resumed".to_string(),
                "project [agents.models] row for '*' is bound to claude-code, not codex — not \
                 applied"
                    .to_string(),
            ]
        );
        assert_eq!(
            msg,
            format!(
                "Spawned agent 'reviewer' in {} (harness changed claude-code → codex; previous \
                 session not resumed; project [agents.models] row for '*' is bound to \
                 claude-code, not codex — not applied)",
                tmux::find_window(server.name(), &session_name, "reviewer")
                    .unwrap()
                    .unwrap()
            )
        );
        let target = tmux::find_window(server.name(), &session_name, "reviewer")
            .unwrap()
            .unwrap();
        server.wait_for_pane_text(
            &target,
            "&& codex --no-daemon -a 'never' -s 'danger-full-access' 'Stand by.'",
        );
        let text = tmux::capture_pane(server.name(), &target).unwrap();
        assert!(!text.contains("resume"), "{text}");
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert_eq!(registry.get("reviewer").unwrap().harness, Harness::Codex);
    }

    /// Run `definition` on opencode, played by a stand-in that answers
    /// every call with `answer` and keeps its record in `project_root`, with
    /// the definition projected into the `login` worktree.
    fn configure_opencode(project_root: &Path, definition: &str, answer: &str, exit: i32) {
        project_opencode_definition(&project_root.join("login"), definition);
        configure_harness(project_root, definition, "opencode");
        let pm_dir = paths::pm_dir(project_root);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config.harness.opencode.binary =
            Some(crate::testing::fake_opencode(project_root, answer, exit));
        config
            .agents
            .models
            .entry(definition.to_string())
            .or_insert_with(|| "local/qwen".to_string());
        config.save(&pm_dir).unwrap();
    }

    /// `definition` as `pm upgrade` projects it for opencode into `worktree`.
    pub(crate) fn project_opencode_definition(worktree: &Path, definition: &str) {
        let agents = worktree.join(Harness::OpenCode.config_dir()).join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join(format!("{definition}.md")), "# stub").unwrap();
    }

    /// The config file the stand-in TUI was launched with.
    fn opencode_config(project_root: &Path) -> serde_json::Value {
        let env = std::fs::read_to_string(project_root.join("env")).unwrap();
        let path = env
            .lines()
            .find_map(|line| line.strip_prefix("OPENCODE_CONFIG="))
            .unwrap();
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Wait for the window's shell to have launched the stand-in TUI, and
    /// return what it was launched with.
    fn opencode_tui_argv(project_root: &Path) -> Vec<String> {
        for _ in 0..500 {
            let argv = crate::testing::fake_opencode_argv(project_root);
            if argv.first().is_some_and(|first| first == "--standalone") {
                return argv;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "the window never launched opencode; last record: {:?}",
            crate::testing::fake_opencode_argv(project_root)
        );
    }

    #[test]
    fn spawn_on_opencode_creates_the_session_before_the_window_and_records_it() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        configure_opencode(dir.path(), "reviewer", r#"{"data":{"id":"ses_abc"}}"#, 0);

        let (outcome, _, _) =
            agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::Spawned);

        // Known at spawn, not reported later by a hook.
        let registry = AgentRegistry::load(&paths::agents_dir(dir.path()), &feature).unwrap();
        let entry = registry.get("reviewer").unwrap();
        assert_eq!(entry.harness, Harness::OpenCode);
        assert_eq!(entry.session_id, "ses_abc");

        assert_eq!(
            opencode_tui_argv(dir.path()),
            ["--standalone", "--auto", "--session", "ses_abc"]
        );
        let target = tmux::find_window(server.name(), &session_name, "reviewer")
            .unwrap()
            .expect("window");
        server.wait_for_pane_text(&target, "'PM_OPENCODE_SESSION=ses_abc'");
    }

    #[test]
    fn spawn_on_opencode_from_inside_another_opencode_agent_uses_its_own_config() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        configure_opencode(dir.path(), "reviewer", r#"{"data":{"id":"ses_abc"}}"#, 0);
        // What every window of this session inherits when the session was
        // started from an opencode agent's shell.
        for (key, value) in [
            ("OPENCODE_CONFIG", "/caller/opencode.json"),
            ("OPENCODE_CONFIG_CONTENT", r#"{"model":"caller/model"}"#),
        ] {
            server.set_session_env(&session_name, key, value);
        }

        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        opencode_tui_argv(dir.path());

        let env = std::fs::read_to_string(dir.path().join("env")).unwrap();
        let (config, content) = env.trim_end().split_once('\n').unwrap();
        assert_eq!(content, "OPENCODE_CONFIG_CONTENT=", "{env}");
        let path = config.strip_prefix("OPENCODE_CONFIG=").unwrap();
        assert_ne!(path, "/caller/opencode.json");
        assert_eq!(opencode_config(dir.path())["model"], "local/qwen");
    }

    #[test]
    fn spawn_on_opencode_renders_the_providers_of_both_config_tiers() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_, feature) = setup_project(dir.path(), &server);
        configure_opencode(dir.path(), "reviewer", r#"{"data":{"id":"ses_abc"}}"#, 0);
        let pm_dir = paths::pm_dir(dir.path());
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config.harness.opencode.providers = [(
            "local".to_string(),
            "package = \"project-pkg\"".parse().unwrap(),
        )]
        .into();
        config.save(&pm_dir).unwrap();
        let global: GlobalConfig = toml::from_str(
            r#"
[harness.opencode.providers.local]
package = "global-pkg"
settings = { baseURL = "http://127.0.0.1:8000/v1" }

[harness.opencode.providers.second]
package = "second-pkg"
"#,
        )
        .unwrap();

        let agents_dir = paths::agents_dir(dir.path());
        std::fs::create_dir_all(&agents_dir).unwrap();
        spawn_session_with_config(
            &SpawnParams {
                project_root: dir.path(),
                feature: &feature,
                agent_name: Some("reviewer"),
                agent_definition: None,
                prompt: None,
                resume_session: None,
                fork_session: false,
                reuse_window: None,
                tmux_server: server.name(),
            },
            &ProjectConfig::load(&pm_dir).unwrap(),
            &global,
        )
        .unwrap();
        opencode_tui_argv(dir.path());

        assert_eq!(
            opencode_config(dir.path()),
            serde_json::json!({
                "model": "local/qwen",
                "enabled_providers": ["local", "second"],
                "providers": {
                    "local": {"package": "project-pkg", "models": {"qwen": {}}},
                    "second": {"package": "second-pkg"},
                },
            })
        );
    }

    #[test]
    fn spawn_on_opencode_without_a_model_row_is_refused_and_says_which_row_was_dropped() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        configure_opencode(dir.path(), "reviewer", r#"{"data":{"id":"ses_abc"}}"#, 0);
        let pm_dir = paths::pm_dir(dir.path());
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config.agents.models.remove("reviewer");
        // Bound to the `*` harness, which is not opencode.
        config
            .agents
            .models
            .insert("*".to_string(), "opus".to_string());
        config.save(&pm_dir).unwrap();

        let err = agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("opencode agent 'reviewer' has no [agents.models] row"),
            "{err}"
        );
        assert!(
            err.ends_with(
                "(project [agents.models] row for '*' is bound to claude-code, not opencode — \
                 not applied)"
            ),
            "{err}"
        );
        assert!(crate::testing::fake_opencode_calls(dir.path()).is_empty());
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_none()
        );
        let registry = AgentRegistry::load(&paths::agents_dir(dir.path()), &feature).unwrap();
        assert!(registry.get("reviewer").is_none());
    }

    #[test]
    fn spawn_on_opencode_pins_the_configured_model_or_refuses_the_row() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        configure_opencode(dir.path(), "reviewer", r#"{"data":{"id":"ses_abc"}}"#, 0);
        let set_model = |model: &str| {
            let pm_dir = paths::pm_dir(dir.path());
            let mut config = ProjectConfig::load(&pm_dir).unwrap();
            config
                .agents
                .models
                .insert("reviewer".to_string(), model.to_string());
            config.save(&pm_dir).unwrap();
        };

        // A row opencode's session API cannot take never reaches opencode.
        set_model("opus");
        let err = agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("must be `<provider>/<model>[#variant]`"),
            "{err}"
        );
        assert!(crate::testing::fake_opencode_calls(dir.path()).is_empty());
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_none()
        );

        // One it can take is pinned on the session, where a model opencode
        // cannot resolve fails the turn instead of running on its default.
        set_model("nowhere/does-not-exist");
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        opencode_tui_argv(dir.path());
        let create = crate::testing::fake_opencode_calls(dir.path()).remove(0);
        let body: serde_json::Value = serde_json::from_str(create.last().unwrap()).unwrap();
        assert_eq!(
            body["model"],
            serde_json::json!({"providerID": "nowhere", "id": "does-not-exist"})
        );
        assert_eq!(
            opencode_config(dir.path())["enabled_providers"],
            serde_json::json!(["nowhere"])
        );
    }

    #[test]
    fn spawn_on_opencode_that_cannot_create_a_session_leaves_nothing() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        configure_opencode(
            dir.path(),
            "reviewer",
            r#"{"_tag":"InvalidRequestError","message":"no such directory"}"#,
            1,
        );

        let err = agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("opencode session.create failed: no such directory"),
            "{err}"
        );
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_none()
        );
        let registry = AgentRegistry::load(&paths::agents_dir(dir.path()), &feature).unwrap();
        assert!(registry.get("reviewer").is_none());
    }

    #[test]
    fn respawn_on_opencode_reopens_the_stored_session_and_keeps_it_recorded() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        configure_opencode(dir.path(), "reviewer", r#"{"data":{"id":"ses_first"}}"#, 0);
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        opencode_tui_argv(dir.path());

        tmux::kill_session(server.name(), &session_name).unwrap();
        tmux::create_session(server.name(), &session_name, &dir.path().join(&feature)).unwrap();
        // A second session would be a fresh agent with no transcript.
        configure_opencode(dir.path(), "reviewer", r#"{"data":{"id":"ses_second"}}"#, 0);
        for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
            if entry.file_name().to_string_lossy().starts_with("argv") {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }

        let (outcome, _, _) =
            agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::Resumed);
        assert_eq!(
            opencode_tui_argv(dir.path()),
            ["--standalone", "--auto", "--session", "ses_first"]
        );
        // No SessionStart hook refills it on opencode before the plugin
        // loads, and a window that dies first must still be resumable.
        let registry = AgentRegistry::load(&paths::agents_dir(dir.path()), &feature).unwrap();
        assert_eq!(registry.get("reviewer").unwrap().session_id, "ses_first");
    }

    #[test]
    fn spawn_errors_on_unsupported_harness_and_leaves_nothing() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        configure_harness(dir.path(), "reviewer", "aider");
        let err =
            agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap_err();
        assert!(
            matches!(err, PmError::HarnessUnsupported { .. }),
            "got: {err}"
        );
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_none()
        );
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
    fn spawn_all_preserves_alias_on_respawn() {
        // After spawning an aliased agent, killing its window, and
        // calling `agent_spawn_all`, the respawn should still launch
        // `claude --agent <definition>` (preserved via the registry).
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn(
            dir.path(),
            &feature,
            "frontend-dev",
            Some("implementer"),
            None,
            server.name(),
        )
        .unwrap();

        // Kill and recreate the session (simulating restart — windows gone)
        tmux::kill_session(server.name(), &session_name).unwrap();
        let worktree = dir.path().join("login");
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(result.spawned_count, 1);

        // Window restored under the display name
        assert!(
            tmux::find_window(server.name(), &session_name, "frontend-dev")
                .unwrap()
                .is_some()
        );

        // Registry still records the definition
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get("frontend-dev").unwrap();
        assert_eq!(entry.agent_definition.as_deref(), Some("implementer"));
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

    #[test]
    fn validate_definition_resolves_with_home_pass_and_fail() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let home = tempdir().unwrap();

        // Missing everywhere; present only in the global agents dir; present
        // only in the main worktree (home = None) — each must resolve correctly.
        assert!(matches!(
            validate_definition_resolves_with_home(project_root, "impl", Some(home.path()))
                .unwrap_err(),
            PmError::AgentDefinitionMissing { .. }
        ));

        let global = home.path().join(".agents/agents");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(global.join("impl.md"), "# stub").unwrap();
        validate_definition_resolves_with_home(project_root, "impl", Some(home.path())).unwrap();

        let main = paths::main_worktree(project_root).join(".agents/agents");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("other.md"), "# stub").unwrap();
        validate_definition_resolves_with_home(project_root, "other", None).unwrap();
    }

    #[test]
    fn vanilla_agent_gets_no_definition_flag() {
        // The reserved name is filtered out of the definition flag; any
        // other definition passes through.
        let alias = "default";
        assert_eq!(definition_flag(Some(alias)), None, "{alias}");
        let cmd = Harness::ClaudeCode.build_cmd(
            &SpawnSpec {
                definition: definition_flag(Some(alias)),
                ..Default::default()
            },
            &HarnessConfig::default(),
            &harness::PreLaunch::default(),
        );
        assert!(
            !cmd.contains("--agent"),
            "vanilla spawn must not pass --agent, got: {cmd}"
        );
        assert_eq!(definition_flag(Some("reviewer")), Some("reviewer"));
    }

    #[test]
    fn vanilla_agent_skips_definition_validation() {
        // `pm agent spawn default` must work with no def anywhere.
        let tmp = tempfile::tempdir().unwrap();
        validate_definition_resolves_with_home(tmp.path(), "default", None).unwrap();
    }

    #[test]
    fn spawning_vanilla_agent_launches_a_plain_session() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        let alias = "default";
        let (outcome, _, _) =
            agent_spawn(dir.path(), &feature, alias, None, None, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::Spawned);
        let target = tmux::find_window(server.name(), &session_name, alias)
            .unwrap()
            .expect("window");
        server.wait_for_pane_text(&target, &format!("PM_AGENT_NAME={alias} && claude"));
        let text = tmux::capture_pane(server.name(), &target).unwrap();
        assert!(!text.contains("--agent"), "{alias}: {text}");
    }

    #[test]
    fn feature_agent_can_edit_the_summary_outside_its_worktree() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn(dir.path(), &feature, "default", None, None, server.name()).unwrap();

        let summaries = paths::summaries_dir(dir.path());
        assert!(summaries.is_dir());
        let target = tmux::find_window(server.name(), &session_name, "default")
            .unwrap()
            .expect("window");
        server.wait_for_pane_text(
            &target,
            &format!("--add-dir='{}' 'Stand by.'", summaries.display()),
        );
    }

    #[test]
    fn writable_dirs_are_pm_state_shared_git_config_dir_and_configured_roots() {
        let root = Path::new("/proj");
        let config = HarnessConfig {
            codex: crate::state::project::CodexConfig {
                writable_roots: Some(vec!["/abs/cache".into(), "main/target".into()]),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            writable_dirs(root, &config),
            vec![
                PathBuf::from("/proj/.pm"),
                PathBuf::from("/proj/main/.git"),
                paths::global_config_dir().unwrap(),
                PathBuf::from("/abs/cache"),
                PathBuf::from("/proj/main/target"),
            ]
        );
    }

    #[test]
    fn window_command_exports_agent_name_for_named_agents_only() {
        assert_eq!(
            window_command(Some("reviewer"), "claude --agent reviewer"),
            "export PM_AGENT_NAME=reviewer && claude --agent reviewer"
        );
        assert_eq!(window_command(None, "claude"), "claude");
    }

    #[test]
    fn spawn_settings_keyed_by_definition_not_display_name() {
        // A named agent (display `backend-dev`, definition `implementer`)
        // takes the definition's settings, not the display name's.
        let project = AgentsConfig {
            models: [
                ("implementer".to_string(), "opus".to_string()),
                ("backend-dev".to_string(), "haiku".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let named = effective_definition(Some("implementer"), Some("backend-dev"));
        let settings = spawn_settings(named, &project, &AgentsConfig::default()).unwrap();
        assert_eq!(settings.model.as_deref(), Some("opus"));

        // With no override the display name doubles as the definition.
        let plain = effective_definition(None, Some("backend-dev"));
        let settings = spawn_settings(plain, &project, &AgentsConfig::default()).unwrap();
        assert_eq!(settings.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn spawn_settings_without_definition_takes_no_config() {
        // A plain claude session has no definition to key on.
        let project = AgentsConfig {
            permissions: [("claude".to_string(), "plan".to_string())]
                .into_iter()
                .collect(),
            models: [("claude".to_string(), "opus".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let settings = spawn_settings(None, &project, &AgentsConfig::default()).unwrap();
        assert_eq!(settings, AgentSettings::default());
    }
}
