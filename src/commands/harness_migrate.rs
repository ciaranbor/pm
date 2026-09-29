//! `pm harness migrate`: make the sessions recorded at a directory's old
//! path resumable at its new one. What that takes is the harness's own
//! ([`Harness::migrate_sessions`]); this handler adds what only pm knows —
//! which sessions an agent is running on right now.

use std::path::Path;

use crate::error::Result;
use crate::harness::{Harness, InUse, SessionStore};
use crate::state::agent::{AgentEntry, AgentRegistry};
use crate::state::paths;
use crate::state::project::{GlobalConfig, HarnessConfig, ProjectConfig, harness_config_in};
use crate::tmux;

pub struct MigrateParams<'a> {
    pub harness: Harness,
    pub from: &'a Path,
    /// The directory the sessions now belong to.
    pub to: &'a Path,
    /// The project `to` is part of, if any.
    pub project_root: Option<&'a Path>,
    pub home: &'a Path,
    /// The global tier's `[harness.*]` settings.
    pub global: &'a HarnessConfig,
    pub tmux_server: Option<&'a str>,
}

pub fn migrate(params: &MigrateParams<'_>) -> Result<Vec<String>> {
    let config = harness_config_in(params.project_root, params.global);
    let store = SessionStore {
        home: params.home,
        config: &config,
    };
    let in_use = params
        .project_root
        .map(|root| sessions_in_use(root, params.harness, params.tmux_server))
        .unwrap_or_default();
    params
        .harness
        .migrate_sessions(&store, params.from, params.to, &in_use)
}

/// A move of `from` to `to` made by another command, whose sessions follow.
pub(super) struct Carry<'a> {
    pub from: &'a Path,
    pub to: &'a Path,
    /// The project `to` is part of; its config names the harnesses.
    pub project_root: &'a Path,
    /// Overrides the user's home.
    pub home: Option<&'a Path>,
    pub tmux_server: Option<&'a str>,
}

/// Carry the sessions of every harness the project's agents run on, and
/// return the report. Nothing here is fatal, since the command this is part
/// of has done its own work by now: a harness whose store cannot be reached
/// is skipped with a line saying so, and a failure is reported with the
/// command that retries it.
pub(super) fn carry_sessions(carry: &Carry<'_>) -> Vec<String> {
    let home = match carry
        .home
        .map(|home| Ok(home.to_path_buf()))
        .unwrap_or_else(paths::home_dir)
    {
        Ok(home) => home,
        Err(e) => return vec![format!("Warning: no sessions were carried: {e}")],
    };
    let global = GlobalConfig::load_or_default().harness;
    let config = harness_config_in(Some(carry.project_root), &global);
    let harnesses = super::skills::harnesses_in_use(carry.project_root)
        .unwrap_or_else(|_| vec![Harness::default()]);

    let mut report = Vec::new();
    for harness in harnesses {
        if let Some(why) = harness.sessions_unreachable(&config) {
            report.push(format!("Skipped {harness} sessions: {why}"));
            continue;
        }
        let migrated = migrate(&MigrateParams {
            harness,
            from: carry.from,
            to: carry.to,
            project_root: Some(carry.project_root),
            home: &home,
            global: &global,
            tmux_server: carry.tmux_server,
        });
        match migrated {
            Ok(messages) => report.extend(messages),
            Err(e) => {
                report.push(format!(
                    "Warning: {harness} sessions of {} were not all carried to {}: {e}",
                    carry.from.display(),
                    carry.to.display()
                ));
                report.push(format!(
                    "To retry, run `pm harness migrate --harness {harness} --from {}` in {}",
                    carry.from.display(),
                    carry.to.display()
                ));
            }
        }
    }
    report
}

/// The agents of `scope` that are running: active, with a window.
/// Advisory — state that cannot be read names no agent.
pub(super) fn running_in_scope(
    project_root: &Path,
    project_name: &str,
    scope: &str,
    tmux_server: Option<&str>,
) -> Vec<(String, AgentEntry)> {
    let Ok(registry) = AgentRegistry::load(&paths::agents_dir(project_root), scope) else {
        return Vec::new();
    };
    let session = tmux::session_name(project_name, scope);
    registry
        .agents
        .into_iter()
        .filter(|(_, entry)| entry.active)
        .filter(|(_, entry)| {
            matches!(
                tmux::find_window(tmux_server, &session, &entry.window_name),
                Ok(Some(_))
            )
        })
        .collect()
}

/// The sessions of the project's agents that are running on `harness`.
fn sessions_in_use(project_root: &Path, harness: Harness, tmux_server: Option<&str>) -> Vec<InUse> {
    let Ok(config) = ProjectConfig::load(&paths::pm_dir(project_root)) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(paths::agents_dir(project_root)) else {
        return Vec::new();
    };

    let mut scopes: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .filter_map(|name| name.strip_suffix(".toml").map(str::to_string))
        .collect();
    scopes.sort();

    scopes
        .iter()
        .flat_map(|scope| {
            running_in_scope(project_root, &config.project.name, scope, tmux_server)
                .into_iter()
                .filter(|(_, entry)| entry.harness == harness && !entry.session_id.is_empty())
                .map(move |(name, entry)| InUse {
                    session_id: entry.session_id,
                    agent: format!("{scope}/{name}"),
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::agent::AgentType;
    use crate::state::project::OpenCodeConfig;
    use crate::testing::{TestServer, fake_opencode_calls, fake_opencode_sequence};
    use tempfile::tempdir;

    fn opencode_agent(session_id: &str, window: &str, active: bool) -> AgentEntry {
        AgentEntry {
            agent_type: AgentType::Agent,
            session_id: session_id.to_string(),
            window_name: window.to_string(),
            active,
            agent_definition: None,
            harness: Harness::OpenCode,
            spawned_at: None,
        }
    }

    #[test]
    fn carry_covers_every_harness_the_project_runs_agents_on() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        let from = dir.path().join("elsewhere");
        let to = project_path.canonicalize().unwrap().join("login");
        std::fs::create_dir_all(&to).unwrap();

        let claude = dir
            .path()
            .join(".claude/projects")
            .join(crate::testing::claude_key(&from));
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join("abc.jsonl"), "{}\n").unwrap();

        let fake = tempdir().unwrap();
        let listing = r#"{"data":[{"id":"ses_a"}],"cursor":{"previous":null,"next":null}}"#;
        let bound = serde_json::json!({
            "data": {"location": {"directory": to.canonicalize().unwrap()}},
        })
        .to_string();
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        for (agent, harness) in [("qa", "opencode"), ("reviewer", "codex")] {
            config
                .agents
                .harness
                .insert(agent.to_string(), harness.to_string());
        }
        config.harness.opencode.binary = Some(fake_opencode_sequence(
            fake.path(),
            &[
                "opencode v2.0.18",
                listing,
                "server listening on http://127.0.0.1:9",
                "",
                &bound,
            ],
            0,
        ));
        config.save(&pm_dir).unwrap();

        let report = carry_sessions(&Carry {
            from: &from,
            to: &to,
            project_root: &project_path,
            home: Some(dir.path()),
            tmux_server: server.name(),
        });

        assert!(
            dir.path()
                .join(".claude/projects")
                .join(crate::testing::claude_key(&to))
                .join("abc.jsonl")
                .exists()
        );
        assert!(
            fake_opencode_calls(fake.path())
                .iter()
                .any(|call| call.iter().any(|arg| arg == "sessionID=ses_a")
                    && call.iter().any(|arg| arg == "session.move"))
        );
        assert_eq!(
            report,
            [
                format!(
                    "Copied 1 Claude session(s) from {} to {}",
                    from.display(),
                    to.display()
                ),
                format!(
                    "Moved 1 opencode session(s) from {} to {}",
                    from.display(),
                    to.display()
                ),
                "Nothing to migrate: codex finds a session by id, and pm resumes it in the \
                 agent's current directory"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn migrate_moves_all_but_the_sessions_of_running_agents() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        let worktree = project_path.join("login");
        std::fs::create_dir_all(&worktree).unwrap();

        // `reviewer` has a window; `tester` is active but its window is
        // gone; `stopped` has a window of that name left over.
        let session = tmux::session_name(&project_name, "login");
        tmux::create_session(server.name(), &session, &worktree).unwrap();
        for window in ["reviewer", "stopped"] {
            tmux::new_window(server.name(), &session, &worktree, Some(window), true).unwrap();
        }
        let mut registry = AgentRegistry::default();
        registry.register("reviewer", opencode_agent("ses_running", "reviewer", true));
        registry.register("tester", opencode_agent("ses_dead", "tester", true));
        registry.register("stopped", opencode_agent("ses_stopped", "stopped", false));
        registry
            .save(&paths::agents_dir(&project_path), "login")
            .unwrap();

        let fake = tempdir().unwrap();
        let listing = serde_json::json!({
            "data": [{"id": "ses_running"}, {"id": "ses_dead"}, {"id": "ses_stopped"}],
            "cursor": {"previous": null, "next": null},
        })
        .to_string();
        let bound = serde_json::json!({
            "data": {"location": {"directory": worktree.canonicalize().unwrap()}},
        })
        .to_string();
        let global = HarnessConfig {
            opencode: OpenCodeConfig {
                binary: Some(fake_opencode_sequence(
                    fake.path(),
                    &[
                        &listing,
                        "server listening on http://127.0.0.1:9",
                        "",
                        &bound,
                        "",
                        &bound,
                    ],
                    0,
                )),
                ..Default::default()
            },
            ..Default::default()
        };

        let messages = migrate(&MigrateParams {
            harness: Harness::OpenCode,
            from: Path::new("/gone/old-login"),
            to: &worktree,
            project_root: Some(&project_path),
            home: dir.path(),
            global: &global,
            tmux_server: server.name(),
        })
        .unwrap();
        tmux::kill_session(server.name(), &session).unwrap();

        let moved: Vec<String> = fake_opencode_calls(fake.path())
            .into_iter()
            .filter(|call| call.iter().any(|arg| arg == "session.move"))
            .filter_map(|call| {
                call.iter()
                    .find_map(|arg| arg.strip_prefix("sessionID=").map(str::to_string))
            })
            .collect();
        assert_eq!(moved, ["ses_dead", "ses_stopped"]);
        assert!(
            messages[0].contains("ses_running") && messages[0].contains("login/reviewer"),
            "{messages:?}"
        );
    }
}
