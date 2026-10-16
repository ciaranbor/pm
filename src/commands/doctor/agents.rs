//! Active agents in a live session: dead windows and harnesses, stopped
//! loops, stale launches, sessions that never started or reported no wake
//! channel, legacy names.

use std::path::Path;
use std::time::{Duration, SystemTime};

use super::{Fix, FixAction, Issue, IssueKind};
use crate::commands::agent_restart::restart_command;
use crate::commands::attention::{self, AgentState};
use crate::commands::running_agents::Windows;
use crate::commands::{launch_stamp, vanilla_rename};
use crate::error::Result;
use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig};
use crate::state::runtime;
use crate::state::workflow::{LEGACY_VANILLA_AGENT, VANILLA_AGENT};

/// How long after its spawn an agent may go without its session starting,
/// or a loaded never-idle loop, before that is reported.
pub(crate) const START_GRACE: Duration = Duration::from_secs(60);

/// Findings about `scope`'s active agents, given its tmux session exists.
pub(super) fn agent_issues(
    project_root: &Path,
    project_name: &str,
    scope: &str,
    session_name: &str,
    tmux_server: Option<&str>,
) -> Result<Vec<Issue>> {
    let agents_dir = paths::agents_dir(project_root);
    let Ok(registry) = AgentRegistry::load(&agents_dir, scope) else {
        return Ok(Vec::new());
    };
    // An entry with no spawn time is at least as old as the file holding it.
    let registry_written = AgentRegistry::modified(&agents_dir, scope);
    let past_grace = |entry: &AgentEntry| {
        entry
            .spawned_at
            .map(SystemTime::from)
            .or(registry_written)
            .and_then(|since| since.elapsed().ok())
            .is_some_and(|age| age > START_GRACE)
    };
    let windows = Windows::read(tmux_server)?;
    // What the attention snapshot reads each agent as; unreadable, none.
    let states = attention::scope_agents_in(project_root, scope, &windows).unwrap_or_default();
    let config = ProjectConfig::load(&paths::pm_dir(project_root)).ok();
    let global = GlobalConfig::load_or_default();
    let mut issues = Vec::new();
    for (agent_name, entry) in &registry.agents {
        if entry.agent_type != AgentType::Agent || !entry.active {
            continue;
        }
        let restart = restart_command(project_name, scope, agent_name);
        if windows.find(session_name, &entry.window_name).is_none() {
            issues.push(Issue {
                kind: IssueKind::AgentWindowMissing,
                message: format!("agent '{agent_name}' registered as active but window missing"),
                fix: Fix::Auto(FixAction::RespawnAgent {
                    agent_name: agent_name.clone(),
                }),
            });
            continue;
        }
        if states
            .iter()
            .any(|a| a.name == *agent_name && a.state == AgentState::Dead)
        {
            issues.push(Issue {
                kind: IssueKind::AgentHarnessExited,
                message: format!(
                    "agent '{agent_name}' has its window open but its {} harness exited \
                     (its window shows why; fix that, then {restart})",
                    entry.harness
                ),
                fix: Fix::None,
            });
            continue;
        }
        issues.extend(loop_issue(
            project_root,
            project_name,
            scope,
            agent_name,
            entry,
        ));
        if let Some(config) = &config
            && launch_stamp::is_stale(project_root, scope, agent_name, entry, config, &global)
                .unwrap_or(false)
        {
            issues.push(Issue {
                kind: IssueKind::AgentLaunchStale,
                message: format!(
                    "agent '{agent_name}' runs on what it was launched with, which has changed \
                     since (its definition, the baseline, a notice board, a config row, or pm's \
                     hooks); {restart} relaunches it"
                ),
                fix: Fix::None,
            });
        }
        if past_grace(entry) {
            // First: opencode's plugin is what stamps its session started.
            if entry.harness.loop_loaded(project_root, scope, agent_name) == Some(false) {
                issues.push(Issue {
                    kind: IssueKind::LoopNotLoaded,
                    message: format!(
                        "agent '{agent_name}' is running but {} has not loaded pm's never-idle \
                         plugin, so it never wakes for messages (`/plugins` in its window shows \
                         why; then {restart})",
                        entry.harness
                    ),
                    fix: Fix::None,
                });
            } else if !session_started(project_root, scope, agent_name) {
                issues.push(Issue {
                    kind: IssueKind::AgentSessionNotStarted,
                    message: format!(
                        "agent '{agent_name}' is running but its {} session never reported \
                         starting (run {restart})",
                        entry.harness
                    ),
                    fix: Fix::None,
                });
            } else if runtime::started_at(project_root, scope, agent_name).is_some()
                && runtime::read_inbound(project_root, scope, agent_name).is_none()
            {
                issues.push(Issue {
                    kind: IssueKind::NoWakeChannel,
                    message: format!(
                        "agent '{agent_name}' is running but its {} session reported no wake \
                         channel, so once it stops listening nothing wakes it for messages \
                         until it is prompted or restarted ({restart} starts a session that \
                         may report one)",
                        entry.harness
                    ),
                    fix: Fix::None,
                });
            }
        }
    }
    Ok(issues)
}

/// Whether `agent`'s harness session reported starting since its launch.
/// An agent with no launched stamp was launched by a pm that wrote no start
/// stamp either, so it counts as started.
fn session_started(project_root: &Path, scope: &str, agent: &str) -> bool {
    runtime::started_at(project_root, scope, agent).is_some()
        || runtime::launched_at(project_root, scope, agent).is_none()
}

/// Whether `agent`'s never-idle loop stopped itself, or its last turn
/// failed while the loop still retries. Not fixable here: a restart re-arms
/// the loop, but whatever stopped it (a model that fails every turn, an
/// inbox the agent cannot read) would stop it again.
fn loop_issue(
    project_root: &Path,
    project_name: &str,
    scope: &str,
    name: &str,
    entry: &AgentEntry,
) -> Option<Issue> {
    // A stopped loop's reason already carries the last turn's error.
    if let Some(reason) = entry.harness.loop_stopped(project_root, scope, name) {
        let reason = reason.trim_end_matches('.');
        return Some(Issue {
            kind: IssueKind::LoopStopped,
            message: format!(
                "agent '{name}' no longer wakes for messages — its never-idle loop \
                 stopped: {reason}. Fix the cause, then {}",
                restart_command(project_name, scope, name)
            ),
            fix: Fix::None,
        });
    }
    let error = entry.harness.last_turn_error(project_root, scope, name)?;
    Some(Issue {
        kind: IssueKind::TurnFailed,
        message: format!(
            "agent '{name}' failed its last turn: {error}. Its loop retries, and \
             stops if turns keep failing"
        ),
        fix: Fix::None,
    })
}

/// One warning per agent in `scope` registered as `default` before this
/// project's `plain` migration.
pub(super) fn legacy_vanilla_agent_issues(project_root: &Path, scope: &str) -> Vec<Issue> {
    let Ok(registry) = AgentRegistry::load(&paths::agents_dir(project_root), scope) else {
        return Vec::new();
    };
    if vanilla_rename::is_migrated(project_root) {
        return Vec::new();
    }
    registry
        .agents
        .iter()
        .filter(|(name, entry)| {
            entry.agent_type == AgentType::Agent
                && entry.effective_definition(name) == LEGACY_VANILLA_AGENT
        })
        .map(|(name, _)| Issue {
            kind: IssueKind::LegacyVanillaAgentName,
            message: format!(
                "agent '{name}' was spawned as vanilla agent '{LEGACY_VANILLA_AGENT}', now \
                 '{VANILLA_AGENT}': it cannot be restarted until it is migrated (run `pm \
                 upgrade`)"
            ),
            fix: Fix::Auto(FixAction::MigrateVanillaAgents),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::test_support::*;
    use crate::commands::doctor::{Depth, Finding, diagnose, doctor};
    use crate::harness::Harness;
    use crate::state::project::HarnessConfig;
    use crate::testing::{TestServer, agent_entry};
    use crate::tmux;
    use tempfile::tempdir;

    #[test]
    fn stopped_loop_and_failed_turn_are_reported_until_the_agent_respawns() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        let entry = |harness| crate::state::agent::AgentEntry {
            session_id: "ses_1".to_string(),
            harness,
            ..agent_entry("reviewer")
        };
        let opencode = entry(Harness::OpenCode);
        let native = entry(Harness::ClaudeCode);
        let loop_issues = |scope, entry| {
            Vec::from_iter(loop_issue(&project_path, "app", scope, "reviewer", entry))
        };

        assert!(loop_issues("login", &opencode).is_empty());

        // Written where the spawn told the plugin to write it.
        let spawn = || {
            Harness::OpenCode
                .pre_launch(
                    &crate::harness::LaunchContext {
                        project_root: &project_path,
                        feature: "login",
                        worktree: &project_path,
                        agent: "reviewer",
                    },
                    &crate::harness::SpawnSpec {
                        resume_session: Some("ses_1"),
                        model: Some("local/qwen"),
                        ..Default::default()
                    },
                    &HarnessConfig {
                        opencode: crate::state::project::OpenCodeConfig {
                            binary: Some(crate::testing::fake_opencode(
                                dir.path(),
                                r#"{"data":{"id":"ses_1"}}"#,
                                0,
                            )),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .unwrap()
        };
        let pre = spawn();
        let (_, trip_file) = pre
            .env
            .iter()
            .find(|(key, _)| key == "PM_OPENCODE_TRIP_FILE")
            .expect("the spawn names a trip file");
        // The plugin writes the turn error beside the trip file.
        let turn_error_file = Path::new(trip_file).with_file_name("opencode.turn-error");

        std::fs::write(
            &turn_error_file,
            "Model unavailable: local/qwen (provider.no-route)\n",
        )
        .unwrap();
        let issues = loop_issues("login", &opencode);
        assert_eq!(
            messages(&issues, IssueKind::TurnFailed),
            vec![
                "agent 'reviewer' failed its last turn: Model unavailable: local/qwen \
                 (provider.no-route). Its loop retries, and stops if turns keep failing"
            ]
        );

        // The trip reason already names the last turn's error.
        std::fs::write(trip_file, "5 consecutive turns read none\n").unwrap();
        let issues = loop_issues("login", &opencode);
        assert_eq!(
            messages(&issues, IssueKind::LoopStopped),
            vec![
                "agent 'reviewer' no longer wakes for messages — its never-idle loop stopped: 5 \
                 consecutive turns read none. Fix the cause, then `pm agent restart reviewer \
                 --scope login --project app`"
            ]
        );
        assert!(messages(&issues, IssueKind::TurnFailed).is_empty());
        // The same name in another scope, and on a harness with a native hook.
        assert!(loop_issues("signup", &opencode).is_empty());
        assert!(loop_issues("login", &native).is_empty());

        spawn();
        assert!(loop_issues("login", &opencode).is_empty());
    }

    #[test]
    fn unmigrated_default_agent_is_flagged_and_fix_migrates_it() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) =
            server.setup_project_with_feature(dir.path(), "login");
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        registry.register(
            "default",
            crate::state::agent::AgentEntry {
                active: false,
                ..agent_entry("default")
            },
        );
        registry.save(&agents_dir, "login").unwrap();
        let flagged = |lines: &[String]| {
            lines
                .iter()
                .any(|l| l.contains("agent 'default' was spawned as vanilla agent"))
        };

        // A project born after the rename: `default` is an ordinary name.
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(!flagged(&lines), "{lines:?}");

        std::fs::remove_file(paths::migrations_dir(&project_path).join("plain-agent")).unwrap();
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(flagged(&lines), "{lines:?}");

        doctor(&project_path, &projects_dir, true, server.name()).unwrap();
        let entry = AgentRegistry::load(&agents_dir, "login").unwrap();
        assert_eq!(
            entry
                .get("default")
                .unwrap()
                .effective_definition("default"),
            VANILLA_AGENT
        );
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(!flagged(&lines), "{lines:?}");
    }

    #[test]
    fn detects_dead_agent_window_in_existing_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) =
            server.setup_project_with_feature(dir.path(), "login");

        // Register an agent as active, but don't create its window
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            crate::state::agent::AgentEntry {
                harness: crate::harness::Harness::OpenCode,
                ..agent_entry("reviewer")
            },
        );
        registry.save(&agents_dir, "login").unwrap();
        // A loop stopped in the dead window is moot until it respawns.
        let runtime = crate::state::runtime::agent_dir(&project_path, "login", "reviewer").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("opencode.tripped"),
            "5 consecutive turns read none\n",
        )
        .unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("agent 'reviewer'") && l.contains("window missing")),
            "got: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("no longer wakes")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn fix_respawns_dead_agent() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) =
            server.setup_project_with_feature(dir.path(), "login");

        // Register an agent as active, but don't create its window
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register("reviewer", agent_entry("reviewer"));
        registry.save(&agents_dir, "login").unwrap();

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("agent 'reviewer'")),
            "got: {lines:?}"
        );

        // Agent window should now exist
        assert!(
            tmux::find_window(
                server.name(),
                &tmux::session_name(&project_name, "login"),
                "reviewer"
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn fix_respawn_reports_spawn_notes() {
        let _guard = crate::testing::CODEX_CONFIG_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) =
            server.setup_project_with_feature(dir.path(), "login");

        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            crate::state::agent::AgentEntry {
                session_id: "cc-session".to_string(),
                ..agent_entry("reviewer")
            },
        );
        registry.save(&agents_dir, "login").unwrap();
        // Config moves reviewer off the harness its session was recorded under.
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".to_string(), "codex".to_string());
        config.save(&pm_dir).unwrap();

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines.iter().any(|l| l.contains(
                "login — fixed: agent 'reviewer' registered as active but window missing \
                 (harness changed claude-code → codex; previous session not resumed)"
            )),
            "got: {lines:?}"
        );
    }

    #[test]
    fn healthy_agent_not_flagged() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) =
            server.setup_project_with_feature(dir.path(), "login");

        // Register an agent AND create its window with a non-shell process
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session_name, "login", "reviewer");

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(lines[0].contains("all healthy"), "got: {lines:?}");
    }

    #[test]
    fn running_agent_launched_with_what_has_changed_is_flagged() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) =
            server.setup_project_with_feature(dir.path(), "login");
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session_name, "login", "reviewer");
        crate::state::runtime::write_launch_stamp(&project_path, "login", "reviewer", "old")
            .unwrap();

        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        let stale: Vec<&str> = findings
            .iter()
            .flat_map(Finding::issues)
            .filter(|i| i.kind() == IssueKind::AgentLaunchStale)
            .map(Issue::message)
            .collect();
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(
            stale[0].contains(&format!(
                "`pm agent restart reviewer --scope login --project {project_name}`"
            )),
            "{stale:?}"
        );
    }

    #[test]
    fn running_opencode_agent_whose_plugin_never_loaded_is_flagged_once_past_grace() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) =
            server.setup_project_with_feature(dir.path(), "login");
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session_name, "login", "reviewer");

        let agents_dir = paths::agents_dir(&project_path);
        let save = |edit: &dyn Fn(&mut AgentEntry)| {
            let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
            edit(registry.get_mut("reviewer").unwrap());
            registry.save(&agents_dir, "login").unwrap();
        };
        let login_issues = || -> Vec<(IssueKind, String)> {
            diagnose(&project_path, &projects_dir, server.name(), Depth::Quick)
                .unwrap()
                .iter()
                .filter(|f| f.scope() == "login")
                .flat_map(|f| f.issues())
                .map(|i| (i.kind(), i.message().to_string()))
                .collect()
        };

        // The session id is recorded before the TUI starts, so only the
        // plugin's own marker shows it loaded.
        save(&|e| {
            e.harness = Harness::OpenCode;
            e.session_id = "ses_1".to_string();
            e.spawned_at = Some(chrono::Utc::now());
        });
        assert_eq!(login_issues(), vec![]);

        save(&|e| e.spawned_at = Some(chrono::Utc::now() - 2 * START_GRACE));
        assert_eq!(
            login_issues(),
            vec![(
                IssueKind::LoopNotLoaded,
                format!(
                    "agent 'reviewer' is running but opencode has not loaded pm's never-idle \
                     plugin, so it never wakes for messages (`/plugins` in its window shows why; \
                     then `pm agent restart reviewer --scope login --project {project_name}`)"
                )
            )]
        );

        // Where the plugin writes it.
        let runtime = crate::state::runtime::agent_dir(&project_path, "login", "reviewer").unwrap();
        std::fs::write(runtime.join("opencode.loaded"), "2026-10-01T00:00:00Z\n").unwrap();
        assert_eq!(login_issues(), vec![]);

        // A native hook has nothing to load.
        std::fs::remove_file(runtime.join("opencode.loaded")).unwrap();
        save(&|e| e.harness = Harness::ClaudeCode);
        assert_eq!(login_issues(), vec![]);
    }

    #[test]
    fn an_open_window_whose_harness_exited_is_flagged_instead_of_its_missing_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) =
            server.setup_project_with_feature(dir.path(), "login");
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_dead_fake_agent(&project_path, &session_name, "login", "reviewer");
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        let entry = registry.get_mut("reviewer").unwrap();
        entry.harness = Harness::Codex;
        entry.spawned_at = Some(chrono::Utc::now() - 2 * START_GRACE);
        registry.save(&agents_dir, "login").unwrap();

        let issues: Vec<(IssueKind, String)> =
            diagnose(&project_path, &projects_dir, server.name(), Depth::Quick)
                .unwrap()
                .iter()
                .filter(|f| f.scope() == "login")
                .flat_map(|f| f.issues())
                .map(|i| (i.kind(), i.message().to_string()))
                .collect();
        assert_eq!(
            issues,
            vec![(
                IssueKind::AgentHarnessExited,
                format!(
                    "agent 'reviewer' has its window open but its codex harness exited (its \
                     window shows why; fix that, then `pm agent restart reviewer --scope login \
                     --project {project_name}`)"
                )
            )]
        );
    }

    #[test]
    fn running_agent_whose_session_never_started_is_flagged_once_past_its_own_grace_period() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) =
            server.setup_project_with_feature(dir.path(), "login");
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session_name, "login", "reviewer");

        let agents_dir = paths::agents_dir(&project_path);
        let long_ago = chrono::Utc::now() - 2 * START_GRACE;
        let save = |edit: &dyn Fn(&mut AgentEntry)| {
            let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
            edit(registry.get_mut("reviewer").unwrap());
            registry.save(&agents_dir, "login").unwrap();
        };
        let launched = |at: chrono::DateTime<chrono::Utc>| {
            let file = runtime::reset_launched(&project_path, "login", "reviewer").unwrap();
            std::fs::File::create(file)
                .unwrap()
                .set_modified(at.into())
                .unwrap();
        };
        let login_issues = || -> Vec<(IssueKind, String)> {
            diagnose(&project_path, &projects_dir, server.name(), Depth::Quick)
                .unwrap()
                .iter()
                .filter(|f| f.scope() == "login")
                .flat_map(|f| f.issues())
                .map(|i| (i.kind(), i.message().to_string()))
                .collect()
        };
        let not_started = vec![(
            IssueKind::AgentSessionNotStarted,
            format!(
                "agent 'reviewer' is running but its codex session never reported starting (run \
                 `pm agent restart reviewer --scope login --project {project_name}`)"
            ),
        )];

        // Just spawned: the hook may simply not have fired yet. A resume
        // keeps its session id, so that says nothing about the start.
        save(&|e| {
            e.harness = Harness::Codex;
            e.session_id = "sess-1".to_string();
            e.spawned_at = Some(chrono::Utc::now());
        });
        launched(chrono::Utc::now());
        assert_eq!(login_issues(), vec![]);

        // The registry was written a moment ago, as it is whenever another
        // agent in the scope records its session; the reviewer's own spawn
        // is what counts.
        save(&|e| e.spawned_at = Some(long_ago));
        assert_eq!(login_issues(), not_started);

        save(&|e| e.active = false);
        assert_eq!(login_issues(), vec![]);
        save(&|e| e.active = true);

        runtime::mark_started(&project_path, "login", "reviewer").unwrap();
        assert_eq!(
            login_issues(),
            vec![(
                IssueKind::NoWakeChannel,
                format!(
                    "agent 'reviewer' is running but its codex session reported no wake channel, \
                     so once it stops listening nothing wakes it for messages until it is \
                     prompted or restarted (`pm agent restart reviewer --scope login \
                     --project {project_name}` starts a session that may report one)"
                ),
            )]
        );
        let inbound = runtime::Inbound {
            endpoint: runtime::Endpoint::Socket(dir.path().join("app.sock")),
            token: None,
            thread: Some("sess-1".into()),
        };
        runtime::write_inbound(&project_path, "login", "reviewer", Some(&inbound)).unwrap();
        assert_eq!(login_issues(), vec![]);
        runtime::write_inbound(&project_path, "login", "reviewer", None).unwrap();
        runtime::reset_started(&project_path, "login", "reviewer").unwrap();

        // Launched by a pm that wrote no start stamp.
        runtime::reset_launched(&project_path, "login", "reviewer").unwrap();
        assert_eq!(login_issues(), vec![]);

        // An entry with no spawn time is as old as the registry file.
        launched(chrono::Utc::now());
        save(&|e| e.spawned_at = None);
        assert_eq!(login_issues(), vec![]);
        std::fs::File::options()
            .write(true)
            .open(agents_dir.join("login.toml"))
            .unwrap()
            .set_modified(SystemTime::from(long_ago))
            .unwrap();
        assert_eq!(login_issues(), not_started);
    }
}
