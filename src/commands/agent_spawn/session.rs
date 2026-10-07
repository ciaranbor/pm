//! Launching a harness session in a tmux window, and registering the agent
//! it runs.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::harness::{LaunchContext, SpawnSpec};
use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig};
use crate::state::runtime;
use crate::tmux;

use super::launch::{definition_flag, effective_definition, resolve_launch};

/// The shell line sent to the window: for an agent, its launch stamp
/// ([`runtime::launched_at`]) created, then its identity — its worktree
/// ([`paths::AGENT_WORKTREE_ENV`]) and `PM_AGENT_NAME` (so `pm msg` calls
/// auto-identify) — exported ahead of the harness command.
fn window_command(name: &str, launched: &Path, worktree: &Path, cmd: &str) -> String {
    format!(
        "touch {} && export {}={} PM_AGENT_NAME={name} && {cmd}",
        tmux::shell_quote(&launched.to_string_lossy()),
        paths::AGENT_WORKTREE_ENV,
        tmux::shell_quote(&worktree.to_string_lossy())
    )
}

/// The prompt a named agent is launched with when none is given, so its
/// first turn ends at once and the Stop hook takes over. It is not the
/// user's input (see [`hooks_user_prompt`](crate::commands::hooks_user_prompt)).
pub const SPAWN_PROMPT: &str = "Stand by.";

/// [`SPAWN_PROMPT`] for a resumed session, whose conversation may hold work
/// the agent was told to resume: "Stand by." there reads as the user calling
/// that work off.
pub const RESUME_PROMPT: &str = "pm resumed this session. This is not a message from the user \
     and changes nothing: carry on as your messages direct.";

/// Whether `prompt` is one pm launches an agent with, not the user's input.
pub fn is_launch_prompt(prompt: &str) -> bool {
    let prompt = prompt.trim();
    prompt == SPAWN_PROMPT || prompt == RESUME_PROMPT
}

/// Parameters for spawning an agent session in a tmux window.
pub struct SpawnParams<'a> {
    pub project_root: &'a Path,
    pub feature: &'a str,
    /// Display name for the agent. Used as the tmux window name, the
    /// `PM_AGENT_NAME` env var (so `pm msg` calls auto-identify), and the
    /// registry key.
    pub agent_name: &'a str,
    /// Agent definition to launch, defaulting to `agent_name` (back-compat:
    /// display name doubles as definition name). When `Some(def)` you get a
    /// "named agent": registry key / window / `PM_AGENT_NAME` are all
    /// `agent_name`, but the harness launches definition `def`.
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

/// Spawn an agent session in a tmux window. If `resume_session` is
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
pub(super) fn spawn_session_with_config(
    params: &SpawnParams<'_>,
    config: &ProjectConfig,
    global: &GlobalConfig,
) -> Result<SpawnedSession> {
    let session_name = tmux::session_name(&config.project.name, params.feature);
    let worktree_path = params.project_root.join(params.feature);

    let effective_definition = effective_definition(params.agent_definition, params.agent_name);

    let launch = resolve_launch(
        params.project_root,
        params.feature,
        effective_definition,
        config,
        global,
    )?;
    let settings = &launch.settings;

    // An agent needs a sentinel prompt when none is explicitly provided:
    // a harness with no positional prompt just waits for user input and never
    // completes a turn, so the Stop hook never fires. A trivial "continue"
    // prompt causes an immediate first turn, whose end starts pm's waiter.
    let effective_prompt = match params.prompt {
        Some(p) => p,
        None if params.resume_session.is_some() && !params.fork_session => RESUME_PROMPT,
        None => SPAWN_PROMPT,
    };

    let name = params.agent_name;

    // Compose the single `--append-system-prompt-file`: shared baseline plus
    // any non-empty notice boards. When no board has content this returns the
    // baseline path unchanged (or None when the baseline is also absent), so
    // older projects keep spawning exactly as before.
    let append_file =
        crate::notice::compose_spawn_prompt(params.project_root, params.feature, name)?;
    for dir in &launch.edit_dirs {
        std::fs::create_dir_all(dir)?;
    }
    // A harness with a directory-trust gate would otherwise stop at an
    // interactive prompt nobody is watching.
    settings
        .harness
        .trust_worktree(&paths::home_dir()?, &worktree_path)?;
    let spec = SpawnSpec {
        definition: definition_flag(effective_definition),
        append_prompt_file: append_file.as_deref(),
        prompt: Some(effective_prompt),
        resume_session: params.resume_session,
        fork_session: params.fork_session,
        permission_mode: settings.permission_mode.as_deref(),
        model: settings.model.as_deref(),
        writable_dirs: &launch.writable_dirs,
        edit_dirs: &launch.edit_dirs,
    };
    let pre = settings
        .harness
        .pre_launch(
            &LaunchContext {
                project_root: params.project_root,
                feature: params.feature,
                worktree: &worktree_path,
                agent: name,
            },
            &spec,
            &launch.harness_config,
        )
        // A refusal for want of a model row may be about one the
        // resolution dropped.
        .map_err(|e| match (e, &settings.model, &settings.dropped_model) {
            (PmError::Agent(message), None, Some(dropped)) => {
                PmError::Agent(format!("{message} ({dropped})"))
            }
            (e, _, _) => e,
        })?;
    let cmd = settings
        .harness
        .build_cmd(&spec, &launch.harness_config, &pre);
    let window_target = if let Some(target) = params.reuse_window {
        tmux::rename_window(params.tmux_server, target, name)?;
        target.to_string()
    } else {
        tmux::new_window(
            params.tmux_server,
            &session_name,
            &worktree_path,
            Some(name),
            true,
        )?
    };
    tmux::mark_agent_pane(params.tmux_server, &window_target)?;

    // Register before the command is sent: the harness's SessionStart hook
    // reads the entry, and a hook that fires first would find no agent —
    // on codex that means no role and no baseline, silently.
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
            session_id: registered_session(&pre.session_id, &spec),
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
    // before the session (README, "Follow what needs you").
    let startup = runtime::Waiting::now(runtime::WaitingKind::Startup, None);
    runtime::write_waiting(params.project_root, params.feature, name, &startup)?;
    runtime::reset_loop(params.project_root, params.feature, name)?;
    let stamp =
        crate::commands::launch_stamp::stamp(params.project_root, effective_definition, &launch)?;
    runtime::write_launch_stamp(params.project_root, params.feature, name, &stamp)?;
    runtime::clear_restart_at_idle(params.project_root, params.feature, name)?;
    for which in [
        runtime::SessionPath::Transcript,
        runtime::SessionPath::ConfigDir,
    ] {
        runtime::write_session_path(params.project_root, params.feature, name, which, None)?;
    }

    runtime::reset_started(params.project_root, params.feature, name)?;
    let launched = runtime::reset_launched(params.project_root, params.feature, name)?;
    tmux::send_line(
        params.tmux_server,
        &window_target,
        &window_command(name, &launched, &worktree_path, &cmd),
    )?;

    let resumed = match (params.resume_session, &pre.session_id) {
        (Some(_), _) if params.fork_session => false,
        (Some(asked), Some(opened)) => asked == opened,
        (asked, None) => asked.is_some(),
        (None, Some(_)) => false,
    };
    let mut notes = launch.settings.notes;
    notes.extend(pre.notes);
    Ok(SpawnedSession {
        window_target,
        notes,
        resumed,
    })
}

/// The session id to register before launch. A resume keeps the id it
/// resumes, so a harness that exits before its SessionStart hook leaves the
/// conversation resumable; a fork's id is unknown until that hook records it.
fn registered_session(opened: &Option<String>, spec: &SpawnSpec) -> String {
    match (opened, spec.resume_session) {
        (Some(id), _) => id.clone(),
        (None, Some(id)) if !spec.fork_session => id.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_spawn::test_support::*;
    use crate::commands::agent_spawn::*;
    use crate::harness::Harness;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn a_resume_keeps_its_session_id_until_the_harness_reports_one() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.get_mut("reviewer").unwrap().session_id = "sess-abc123".to_string();
        registry.save(&agents_dir, &feature).unwrap();
        tmux::kill_session(server.name(), &session_name).unwrap();
        tmux::create_session(server.name(), &session_name, &dir.path().join("login")).unwrap();

        // The test harness never starts, so no SessionStart hook replaces
        // the id: a relaunch that dies at startup must not lose it.
        let (outcome, _, _) =
            agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        assert_eq!(outcome, SpawnOutcome::Resumed);
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert_eq!(registry.get("reviewer").unwrap().session_id, "sess-abc123");
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
    fn a_spawn_stamps_its_launch_and_a_changed_input_makes_it_stale() {
        let _guard = crate::testing::CODEX_CONFIG_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let root = dir.path();
        let (_, feature) = setup_project(root, &server);
        configure_harness(root, "tester", "codex");
        configure_opencode(root, "implementer", r#"{"data":{"id":"ses_abc"}}"#, 0);
        let agents = ["reviewer", "tester", "implementer"];
        for agent in agents {
            agent_spawn(root, &feature, agent, None, None, server.name()).unwrap();
        }
        let pm_dir = paths::pm_dir(root);
        let stale = |agent: &str| {
            let registry = AgentRegistry::load(&paths::agents_dir(root), &feature).unwrap();
            crate::commands::launch_stamp::is_stale(
                root,
                &feature,
                agent,
                registry.get(agent).unwrap(),
                &ProjectConfig::load(&pm_dir).unwrap(),
                &GlobalConfig::load_or_default(),
            )
            .unwrap()
        };
        let all_stale = || agents.map(stale);
        assert_eq!(all_stale(), [false; 3], "right after their spawn");

        let definition = paths::main_worktree(root).join(".agents/agents/reviewer.md");
        std::fs::write(&definition, "# edited").unwrap();
        assert_eq!(all_stale(), [true, false, false], "an edited definition");
        std::fs::write(&definition, "# stub").unwrap();
        assert_eq!(all_stale(), [false; 3]);

        std::fs::write(pm_dir.join("notices.md"), "Be terse.").unwrap();
        assert_eq!(all_stale(), [true; 3], "a notice board");
        std::fs::remove_file(pm_dir.join("notices.md")).unwrap();

        let edit = |change: &dyn Fn(&mut ProjectConfig)| {
            let mut config = ProjectConfig::load(&pm_dir).unwrap();
            change(&mut config);
            config.save(&pm_dir).unwrap();
        };
        edit(&|c| {
            c.agents
                .models
                .insert("reviewer".to_string(), "opus".to_string());
        });
        assert!(stale("reviewer"), "a model row");
        edit(&|c| {
            c.agents.models.remove("reviewer");
        });
        edit(&|c| c.harness.codex.sandbox = Some("workspace-write".to_string()));
        assert_eq!(all_stale(), [false, true, false], "[harness.codex]");
        edit(&|c| c.harness.codex.sandbox = None);
        assert_eq!(all_stale(), [false; 3]);

        runtime::write_launch_stamp(root, &feature, "reviewer", "").unwrap();
        assert!(stale("reviewer"), "launched with something else");
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
                agent_name: "reviewer",
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
                agent_name: "reviewer",
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
    fn spawning_vanilla_agent_launches_a_plain_session() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        let alias = "plain";
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

        agent_spawn(dir.path(), &feature, "plain", None, None, server.name()).unwrap();

        let summaries = paths::summaries_dir(dir.path());
        assert!(summaries.is_dir());
        let target = tmux::find_window(server.name(), &session_name, "plain")
            .unwrap()
            .expect("window");
        server.wait_for_pane_text(
            &target,
            &format!("--add-dir='{}' 'Stand by.'", summaries.display()),
        );
    }

    #[test]
    fn window_command_exports_agent_name() {
        assert_eq!(
            window_command(
                "reviewer",
                Path::new("/p/.pm/runtime/login/reviewer/launched"),
                Path::new("/p/login"),
                "claude --agent reviewer"
            ),
            "touch '/p/.pm/runtime/login/reviewer/launched' && \
             export PM_AGENT_WORKTREE='/p/login' PM_AGENT_NAME=reviewer && claude --agent reviewer"
        );
    }
}
