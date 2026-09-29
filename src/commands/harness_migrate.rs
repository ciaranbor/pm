//! `pm harness migrate`: make the sessions recorded at a directory's old
//! path resumable at its new one. What that takes is the harness's own
//! ([`Harness::migrate_sessions`]); this handler adds what only pm knows —
//! which sessions an agent is running on right now.

use std::path::Path;

use crate::error::Result;
use crate::harness::{Harness, InUse, SessionStore};
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::{HarnessConfig, ProjectConfig, harness_config_in};
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

/// Carry the Claude Code sessions recorded at `from` over to `to`; `home`
/// overrides the user's home. A failure is reported, never fatal: the
/// command this is part of has other work to finish.
pub(super) fn carry_sessions(from: &Path, to: &Path, home: Option<&Path>) {
    let migrated = home
        .map(|home| Ok(home.to_path_buf()))
        .unwrap_or_else(paths::home_dir)
        .and_then(|home| {
            let store = SessionStore {
                home: &home,
                config: &HarnessConfig::default(),
            };
            Harness::ClaudeCode.migrate_sessions(&store, from, to, &[])
        });
    match migrated {
        Ok(msgs) => {
            for msg in msgs {
                eprintln!("{msg}");
            }
        }
        Err(e) => eprintln!("Warning: Claude session migration failed: {e}"),
    }
}

/// The sessions of the project's agents that are running on `harness`: an
/// active agent whose window exists. Advisory — state that cannot be read
/// names no session.
fn sessions_in_use(project_root: &Path, harness: Harness, tmux_server: Option<&str>) -> Vec<InUse> {
    let Ok(config) = ProjectConfig::load(&paths::pm_dir(project_root)) else {
        return Vec::new();
    };
    let agents_dir = paths::agents_dir(project_root);
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return Vec::new();
    };

    let mut scopes: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .filter_map(|name| name.strip_suffix(".toml").map(str::to_string))
        .collect();
    scopes.sort();

    let mut in_use = Vec::new();
    for scope in scopes {
        let Ok(registry) = AgentRegistry::load(&agents_dir, &scope) else {
            continue;
        };
        let session = tmux::session_name(&config.project.name, &scope);
        for (name, entry) in &registry.agents {
            if entry.harness != harness || !entry.active || entry.session_id.is_empty() {
                continue;
            }
            if let Ok(Some(_)) = tmux::find_window(tmux_server, &session, &entry.window_name) {
                in_use.push(InUse {
                    session_id: entry.session_id.clone(),
                    agent: format!("{scope}/{name}"),
                });
            }
        }
    }
    in_use
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::agent::{AgentEntry, AgentType};
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
