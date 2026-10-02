use std::path::Path;

use crate::commands::agent_spawn::{SpawnOutcome, notes_suffix};
use crate::commands::attention::{AgentState, scope_agents};
use crate::error::{PmError, Result};
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Restart a single agent: kill its tmux window, then respawn it.
/// The `active` flag stays `true` throughout. If the agent has a stored
/// `session_id`, its harness resumes that session on respawn. If the entry
/// records an explicit `agent_definition`, the definition is preserved
/// across the restart (relayed via `agent_spawn` reading the registry).
///
/// When `keep_old` is set the old window is renamed but left running and
/// returned, for a caller whose own process lives in it. When `resume` is
/// set the agent is first sent a message telling it to resume, which its
/// respawned session reads at its first Stop hook.
fn restart_one(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    tmux_server: Option<&str>,
    keep_old: bool,
    resume: bool,
) -> Result<(String, Option<String>)> {
    crate::messages::validate_name(agent_name, "agent")?;

    let pm_dir = paths::pm_dir(project_root);
    let agents_dir = paths::agents_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let session_name = tmux::session_name(&config.project.name, feature);

    let registry = AgentRegistry::load(&agents_dir, feature)?;

    registry.get(agent_name).ok_or_else(|| {
        crate::error::PmError::AgentNotFound(format!(
            "'{agent_name}' not found in scope '{feature}'"
        ))
    })?;

    if resume {
        messages::send(
            &paths::messages_dir(project_root),
            feature,
            agent_name,
            RESUME_SENDER,
            resume_body(keep_old),
        )?;
    }

    // Renamed rather than killed first, so agent_spawn sees no window and
    // creates a fresh one.
    let old_window = tmux::find_window(tmux_server, &session_name, agent_name)?;
    if let Some(ref target) = old_window {
        let temp_name = format!("{agent_name}-restarting");
        let _ = tmux::rename_window(tmux_server, target, &temp_name);
    }

    // `None` for the definition makes agent_spawn re-read the stored one,
    // so an aliased agent keeps its definition.
    let (outcome, _spawn_msg, notes) = super::agent_spawn::agent_spawn(
        project_root,
        feature,
        agent_name,
        None,
        None,
        tmux_server,
    )?;

    // Selected before the old window is killed, so tmux doesn't jump to an
    // arbitrary neighbour.
    if let Some(new_target) = tmux::find_window(tmux_server, &session_name, agent_name)? {
        let _ = tmux::select_window(tmux_server, &new_target);
    }

    let kept = match old_window {
        Some(target) if keep_old => Some(target),
        Some(target) => {
            let _ = tmux::kill_window(tmux_server, &target);
            None
        }
        None => None,
    };
    let mut line = restarted_line(agent_name, outcome, &notes);
    if resume && !keep_old {
        line.push_str("; it was interrupted mid-turn and is told to resume");
    }
    Ok((line, kept))
}

/// The report line for an agent that was stopped and respawned.
pub(super) fn restarted_line(agent_name: &str, outcome: SpawnOutcome, notes: &[String]) -> String {
    let resumed = if outcome == SpawnOutcome::Resumed {
        " (resumed session)"
    } else {
        ""
    };
    format!(
        "Restarted agent '{agent_name}'{resumed}{}",
        notes_suffix(notes)
    )
}

/// Completed restarts, in the order they ran.
#[derive(Debug)]
pub struct Restarted {
    pub results: Vec<Result<String>>,
    /// The old window of the agent the restart ran from, left to
    /// [`Restarted::finish`].
    caller_window: Option<String>,
}

impl Restarted {
    /// Kill the old window the restart ran from, if it ran from one. That
    /// ends the calling process, so it comes after the results are printed.
    pub fn finish(self, tmux_server: Option<&str>) {
        if let Some(window) = self.caller_window {
            let _ = tmux::kill_window(tmux_server, &window);
        }
    }
}

/// The sender of the message that tells an interrupted agent to resume.
const RESUME_SENDER: &str = "no-reply-restart";

/// Restart multiple agents. Continues on error. An agent whose turn the
/// restart would cut short — busy, asking, or running background work — is
/// refused unless `force`, and is then told to resume once it is back. An
/// agent whose window holds this process is restarted last, always told to
/// resume, and its old window left to [`Restarted::finish`]: killing it
/// ends the restart.
pub fn agent_restart_many(
    project_root: &Path,
    feature: &str,
    names: &[String],
    force: bool,
    tmux_server: Option<&str>,
) -> Restarted {
    let caller = callers_agent(project_root, feature, names, tmux_server);
    let states = scope_agents(project_root, feature, tmux_server).unwrap_or_default();
    let interrupts = |name: &str| {
        states
            .iter()
            .find(|a| a.name == name)
            .is_some_and(|a| mid_turn(a.state))
    };
    let mut results: Vec<Result<String>> = names
        .iter()
        .filter(|n| Some(*n) != caller)
        .map(|name| {
            let resume = interrupts(name);
            if resume && !force {
                return Err(PmError::SafetyCheck(format!(
                    "agent '{name}' is mid-turn; wait until it is idle, or pass --force to \
                     interrupt it and have it resume"
                )));
            }
            restart_one(project_root, feature, name, tmux_server, false, resume)
                .map(|(line, _)| line)
        })
        .collect();
    let mut caller_window = None;
    if let Some(name) = caller {
        match restart_one(project_root, feature, name, tmux_server, true, true) {
            Ok((line, kept)) => {
                caller_window = kept;
                results.push(Ok(line));
            }
            Err(e) => results.push(Err(e)),
        }
    }
    Restarted {
        results,
        caller_window,
    }
}

/// Whether a restart would cut short what an agent in `state` is doing.
fn mid_turn(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Busy | AgentState::Asking | AgentState::Background
    )
}

fn resume_body(caller: bool) -> &'static str {
    if caller {
        "`pm agent restart` restarted you mid-turn, as you asked; the restart completed. \
         Resume the task you were working on."
    } else {
        "`pm agent restart --force` restarted you mid-turn. Your last tool call or background \
         task may not have completed: check its effect, then resume the task you were \
         working on."
    }
}

/// The one of `names` whose window this process runs in.
fn callers_agent<'a>(
    project_root: &Path,
    feature: &str,
    names: &'a [String],
    tmux_server: Option<&str>,
) -> Option<&'a String> {
    let config = ProjectConfig::load(&paths::pm_dir(project_root)).ok()?;
    let session_name = tmux::session_name(&config.project.name, feature);
    let pid = std::process::id();
    names.iter().find(|name| {
        let Ok(Some(window)) = tmux::find_window(tmux_server, &session_name, name) else {
            return false;
        };
        tmux::window_processes(tmux_server, &window)
            .unwrap_or_default()
            .iter()
            .any(|p| p.pid == pid)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_spawn;
    use crate::harness::Harness;
    use crate::state::agent::{AgentEntry, AgentType};
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::testing::TestServer;
    use chrono::Utc;
    use tempfile::tempdir;

    fn restart(
        project_root: &Path,
        feature: &str,
        agent_name: &str,
        tmux_server: Option<&str>,
    ) -> Result<String> {
        restart_one(project_root, feature, agent_name, tmux_server, false, false)
            .map(|(line, _)| line)
    }

    fn setup_project(dir: &Path, server: &TestServer) -> (String, String) {
        let root = dir.to_path_buf();
        let pm_dir = root.join(".pm");
        let project_name = server.scope("proj");
        let feature_name = "login";

        std::fs::create_dir_all(pm_dir.join("features")).unwrap();

        let config = ProjectConfig {
            project: crate::state::project::ProjectInfo {
                name: project_name.clone(),
                max_features: None,
            },
            agents: Default::default(),
            harness: Default::default(),
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
            progress: Default::default(),
            blocked_reason: None,
            blocked_by: None,
        };
        state.save(&pm_dir.join("features"), feature_name).unwrap();

        let worktree = root.join(feature_name);
        std::fs::create_dir_all(&worktree).unwrap();

        // Agent definition stubs so pre-spawn validation resolves.
        let agents = paths::main_worktree(&root).join(".agents/agents");
        std::fs::create_dir_all(&agents).unwrap();
        for name in ["reviewer", "implementer"] {
            std::fs::write(agents.join(format!("{name}.md")), "# stub").unwrap();
        }

        let session_name = tmux::session_name(&project_name, feature_name);
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        (session_name, feature_name.to_string())
    }

    #[test]
    fn restart_respawns_agent_window() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn agent first
        agent_spawn::agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap();

        // Verify window exists
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );

        // Restart
        let msg = restart(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("Restarted agent 'reviewer'"));

        // Window should still exist (new one)
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );

        // active flag should still be true
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert!(registry.get("reviewer").unwrap().active);
    }

    #[test]
    fn restart_lands_on_new_window() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn the agent, then create and select a different window so the
        // active window is NOT the agent's at restart time.
        agent_spawn::agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap();

        let other = tmux::new_window(
            server.name(),
            &session_name,
            dir.path(),
            Some("other"),
            true,
        )
        .unwrap();
        tmux::select_window(server.name(), &other).unwrap();
        assert_eq!(
            tmux::active_window_name(server.name(), &session_name).unwrap(),
            Some("other".to_string())
        );

        restart(dir.path(), &feature, "reviewer", server.name()).unwrap();

        // After restart, the client should be focused on the reviewer window.
        assert_eq!(
            tmux::active_window_name(server.name(), &session_name).unwrap(),
            Some("reviewer".to_string())
        );
    }

    #[test]
    fn restart_with_session_id_resumes() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn agent and set a session_id
        agent_spawn::agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap();

        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.get_mut("reviewer").unwrap().session_id = "sess-abc".to_string();
        registry.save(&agents_dir, &feature).unwrap();

        let msg = restart(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("resumed session"));

        // Window should exist
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn restart_after_harness_change_reports_it() {
        let _guard = crate::testing::CODEX_CONFIG_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn::agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name())
            .unwrap();
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.get_mut("reviewer").unwrap().session_id = "cc-session".to_string();
        registry.save(&agents_dir, &feature).unwrap();

        let pm_dir = paths::pm_dir(dir.path());
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".to_string(), "codex".to_string());
        config.save(&pm_dir).unwrap();

        let msg = restart(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert_eq!(
            msg,
            "Restarted agent 'reviewer' (harness changed claude-code → codex; previous session \
             not resumed)"
        );
        let target = tmux::find_window(server.name(), &session_name, "reviewer")
            .unwrap()
            .unwrap();
        server.wait_for_pane_text(
            &target,
            "&& codex --no-daemon -a 'never' -s 'danger-full-access' 'Stand by.'",
        );
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        assert_eq!(registry.get("reviewer").unwrap().harness, Harness::Codex);
    }

    #[test]
    fn restart_without_window_still_spawns() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Register agent without creating a window
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
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, &feature).unwrap();

        let msg = restart(dir.path(), &feature, "reviewer", server.name()).unwrap();
        assert!(msg.contains("Restarted agent 'reviewer'"));
    }

    #[test]
    fn restart_preserves_agent_definition_alias() {
        // After spawning an aliased agent (display name != definition),
        // a restart must preserve the stored definition so the new claude
        // process is launched with the same `--agent <def>` flag.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn::agent_spawn(
            dir.path(),
            &feature,
            "frontend-dev",
            Some("implementer"),
            None,
            server.name(),
        )
        .unwrap();

        let msg = restart(dir.path(), &feature, "frontend-dev", server.name()).unwrap();
        assert!(msg.contains("Restarted agent 'frontend-dev'"));

        // Window still exists under display name
        assert!(
            tmux::find_window(server.name(), &session_name, "frontend-dev")
                .unwrap()
                .is_some()
        );

        // Registry retains the definition
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get("frontend-dev").unwrap();
        assert_eq!(entry.agent_definition.as_deref(), Some("implementer"));
    }

    #[test]
    fn restart_errors_for_unknown_agent() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = restart(dir.path(), &feature, "nonexistent", server.name());
        assert!(result.is_err());
    }

    #[test]
    fn restart_many_continues_on_error() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Register only reviewer
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
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, &feature).unwrap();

        let results = agent_restart_many(
            dir.path(),
            &feature,
            &["reviewer".to_string(), "nonexistent".to_string()],
            false,
            server.name(),
        )
        .results;
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
    }

    #[test]
    fn a_mid_turn_agent_is_restarted_only_with_force_and_then_told_to_resume() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project, &session, "login", "implementer");
        server.spawn_idle_fake_agent(&project, &session, "login", "reviewer");
        let messages_dir = paths::messages_dir(&project);
        let told = |agent: &str| {
            messages::list(&messages_dir, "login", agent, Some(RESUME_SENDER))
                .unwrap()
                .len()
        };

        let results = agent_restart_many(
            &project,
            "login",
            &["implementer".to_string(), "reviewer".to_string()],
            false,
            server.name(),
        )
        .results;
        let refused = results[0].as_ref().unwrap_err().to_string();
        assert!(refused.contains("--force"), "{refused}");
        assert!(results[1].is_ok(), "{:?}", results[1]);
        assert_eq!((told("implementer"), told("reviewer")), (0, 0));
        assert!(
            tmux::pane_processes(server.name(), &format!("{session}:implementer"))
                .unwrap()
                .iter()
                .any(|p| p.command.contains("999")),
            "a refused agent keeps running"
        );

        let results = agent_restart_many(
            &project,
            "login",
            &["implementer".to_string()],
            true,
            server.name(),
        )
        .results;
        let line = results[0].as_ref().unwrap();
        assert!(line.ends_with("is told to resume"), "{line}");
        assert_eq!(told("implementer"), 1);
    }
}
