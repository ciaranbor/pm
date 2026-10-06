//! `pm agent restart --all`: restart every active agent of a scope, or of
//! every scope of every registered project, through the same per-scope
//! restart as `pm agent restart <names>` ([`agent_restart_many`]).
//!
//! Which agents to restart is decided from one scan of the server
//! ([`scope_agents_in`]); each scope's restart reads its agents again, so
//! one that turned mid-turn since is still refused. Idle, unarmed and dead
//! agents restart — a dead one is healed by the respawn. An agent mid-turn,
//! asking, or running background work is skipped unless `force`, which
//! interrupts it and has it resume. An agent whose scope's session is
//! closed — a closed feature, or a project not opened — is skipped:
//! restarting it would open the session. Stopped agents aren't active and
//! aren't listed. The agent this process runs in is never skipped as busy
//! — it asked for the restart — and its scope goes last, so its old pane,
//! which ends this process when killed, outlives every other restart
//! ([`RestartAll::finish`]). Window focus is left alone: a sweep over many
//! scopes must not move the user's clients from window to window.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};
use crate::tmux;

use super::agent_restart::{Restarted, agent_restart_many, callers_agent};
use super::attention::{AgentState, scope_agents_in};
use super::launch_check::{self, Launch};
use super::running_agents::Windows;

/// A scope of a project.
#[derive(Debug, Clone)]
pub struct Scope {
    pub root: PathBuf,
    /// The project's name, as its sessions are prefixed.
    pub project: String,
    pub name: String,
}

impl Scope {
    /// `scope` of the project at `root`.
    pub fn of(root: &Path, scope: &str) -> Result<Self> {
        let config = ProjectConfig::load(&paths::pm_dir(root))?;
        Ok(Self {
            root: root.to_path_buf(),
            project: config.project.name,
            name: scope.to_string(),
        })
    }

    fn label(&self) -> String {
        tmux::session_name(&self.project, &self.name)
    }
}

/// Every scope of the project at `root`: main, then each feature.
pub fn project_scopes(root: &Path) -> Result<Vec<Scope>> {
    let main = Scope::of(root, "main")?;
    let features = FeatureState::list(&paths::features_dir(root))?;
    let mut scopes = vec![main.clone()];
    scopes.extend(features.into_iter().map(|(name, _)| Scope {
        name,
        ..main.clone()
    }));
    Ok(scopes)
}

/// Every scope of every project registered in `projects_dir`, and a
/// skipped report for each project that can't be read.
pub fn global_scopes(projects_dir: &Path) -> Result<(Vec<Scope>, Vec<Report>)> {
    let registry = ProjectEntry::scan(projects_dir)?;
    let mut scopes = Vec::new();
    let mut unread = Vec::new();
    for (name, entry) in registry.projects {
        match project_scopes(&entry.root_path()) {
            Ok(found) => scopes.extend(found),
            Err(e) => unread.push(Report {
                target: name,
                outcome: Outcome::Skipped(format!("Skipped: project unreadable ({e})")),
            }),
        }
    }
    unread.extend(registry.malformed.into_iter().map(|bad| Report {
        target: bad.name,
        outcome: Outcome::Skipped(format!(
            "Skipped: registry entry unreadable ({})",
            bad.error
        )),
    }));
    Ok((scopes, unread))
}

/// Each holds the report's line.
#[derive(Debug)]
pub enum Outcome {
    Restarted(String),
    Skipped(String),
    Failed(String),
}

/// What happened to one agent, or to a whole project or scope.
#[derive(Debug)]
pub struct Report {
    /// The scope's session name, or the project's name.
    pub target: String,
    pub outcome: Outcome,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Outcome::Restarted(line) | Outcome::Skipped(line) | Outcome::Failed(line)) =
            &self.outcome;
        write!(f, "{}: {line}", self.target)
    }
}

/// What a sweep did to each agent.
#[derive(Debug, Default)]
pub struct Sweep {
    /// What was settled before any restart ran, then the restarts in the
    /// order they ran, the caller's last.
    pub reports: Vec<Report>,
    /// How many restarted agents `force` interrupted mid-turn.
    pub interrupted: usize,
}

impl Sweep {
    pub fn count(&self, kind: fn(&Outcome) -> bool) -> usize {
        self.reports.iter().filter(|r| kind(&r.outcome)).count()
    }

    /// `Restarted N, skipped N, failed N`, and how many were interrupted.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "Restarted {}, skipped {}, failed {}",
            self.count(|o| matches!(o, Outcome::Restarted(_))),
            self.count(|o| matches!(o, Outcome::Skipped(_))),
            self.count(|o| matches!(o, Outcome::Failed(_))),
        );
        if self.interrupted > 0 {
            line.push_str(&format!(
                " ({} interrupted mid-turn by --force, each told to resume)",
                self.interrupted
            ));
        }
        line
    }
}

/// The restarts of one scope.
#[derive(Debug)]
struct Batch {
    scope: Scope,
    done: Restarted,
}

/// The restarts [`restart_all`] ran.
#[derive(Debug)]
pub struct RestartAll {
    /// The agents and scopes settled without a restart.
    unrestarted: Vec<Report>,
    batches: Vec<Batch>,
}

impl RestartAll {
    /// Turn the result of each restarted agent whose harness exited at
    /// launch into a failure saying why. Every launch is watched at once.
    pub fn confirm_launches(&mut self, tmux_server: Option<&str>) {
        let launches: Vec<Launch> = self
            .batches
            .iter()
            .flat_map(|b| {
                b.done.launched().map(|agent| Launch {
                    project_root: b.scope.root.clone(),
                    scope: b.scope.name.clone(),
                    agent: agent.to_string(),
                })
            })
            .collect();
        let mut failed = launch_check::confirm_all(&launches, tmux_server);
        for batch in &mut self.batches {
            let (mine, rest) = failed.into_iter().partition(|f| {
                f.launch.project_root == batch.scope.root && f.launch.scope == batch.scope.name
            });
            failed = rest;
            batch.done.record_failures(mine);
        }
    }

    /// What happened to each agent. A restart refused because its agent
    /// was mid-turn by the time it ran is a skip.
    pub fn sweep(&mut self) -> Sweep {
        let mut sweep = Sweep {
            reports: std::mem::take(&mut self.unrestarted),
            interrupted: 0,
        };
        for batch in &mut self.batches {
            let results = std::mem::take(&mut batch.done.results);
            for (agent, result) in batch.done.agents.iter().zip(results) {
                let refused = batch.done.refused.iter().find(|(a, _)| a == agent);
                let outcome = match (result, refused) {
                    (Ok(line), _) => {
                        sweep.interrupted += usize::from(batch.done.interrupted.contains(agent));
                        Outcome::Restarted(line)
                    }
                    (Err(_), Some((_, why))) => Outcome::Skipped(format!(
                        "Skipped agent '{agent}': {why}; --force interrupts it and has it resume"
                    )),
                    (Err(PmError::Agent(why)), None) => {
                        let why = why
                            .strip_prefix(&format!("agent '{agent}': "))
                            .unwrap_or(&why);
                        Outcome::Failed(format!("Failed to restart agent '{agent}': {why}"))
                    }
                    (Err(e), None) => {
                        Outcome::Failed(format!("Failed to restart agent '{agent}': {e}"))
                    }
                };
                sweep.reports.push(Report {
                    target: batch.scope.label(),
                    outcome,
                });
            }
        }
        sweep
    }

    /// Kill the old pane of the agent this process runs in, if it was
    /// restarted: that ends the process, so it comes after the reports
    /// are printed.
    pub fn finish(self, tmux_server: Option<&str>) {
        for batch in self.batches {
            batch.done.finish(tmux_server);
        }
    }
}

/// Restart every active agent of `scopes` (module docs).
pub fn restart_all(scopes: &[Scope], force: bool, tmux_server: Option<&str>) -> Result<RestartAll> {
    let windows = Windows::read(tmux_server)?;
    let mut unrestarted = Vec::new();
    let mut planned: Vec<(&Scope, Vec<String>, bool)> = Vec::new();
    for scope in scopes {
        let agents = match scope_agents_in(&scope.root, &scope.name, &windows) {
            Ok(agents) => agents,
            Err(e) => {
                unrestarted.push(Report {
                    target: scope.label(),
                    outcome: Outcome::Failed(format!("Failed to read its agents: {e}")),
                });
                continue;
            }
        };
        let active: Vec<String> = agents
            .iter()
            .filter(|a| a.state != AgentState::Stopped)
            .map(|a| a.name.clone())
            .collect();
        let caller = callers_agent(&scope.root, &scope.name, &active, tmux_server);
        let mut names = Vec::new();
        for agent in agents {
            let skip = |why: &str| Report {
                target: scope.label(),
                outcome: Outcome::Skipped(format!("Skipped agent '{}': {why}", agent.name)),
            };
            match agent.state {
                AgentState::Stopped => {}
                AgentState::Closed => unrestarted.push(skip("its session is closed")),
                _ if Some(&agent.name) == caller => names.push(agent.name),
                AgentState::Busy | AgentState::Asking | AgentState::Background if !force => {
                    unrestarted.push(skip(&format!(
                        "{}; --force interrupts it and has it resume",
                        mid_turn(agent.state)
                    )));
                }
                _ => names.push(agent.name),
            }
        }
        if !names.is_empty() {
            planned.push((scope, names, caller.is_some()));
        }
    }
    planned.sort_by_key(|(.., caller)| *caller);

    let batches = planned
        .into_iter()
        .map(|(scope, names, _)| Batch {
            done: agent_restart_many(&scope.root, &scope.name, &names, force, false, tmux_server),
            scope: scope.clone(),
        })
        .collect();
    Ok(RestartAll {
        unrestarted,
        batches,
    })
}

/// Why restarting an agent in `state` would cut its turn short.
fn mid_turn(state: AgentState) -> &'static str {
    match state {
        AgentState::Asking => "a dialog waits on the user",
        AgentState::Background => "it is waiting on background work",
        _ => "it is mid-turn",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn lines(sweep: &Sweep) -> Vec<String> {
        sweep.reports.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn all_restarts_idle_and_dead_agents_and_skips_one_mid_turn() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project, &session, "login", "implementer");
        server.spawn_idle_fake_agent(&project, &session, "login", "reviewer");
        server.spawn_dead_fake_agent(&project, &session, "login", "qa");

        let scope = Scope::of(&project, "login").unwrap();
        let sweep = restart_all(&[scope], false, server.name()).unwrap().sweep();

        let lines = lines(&sweep);
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert_eq!(
            lines[0],
            format!(
                "{session}: Skipped agent 'implementer': it is mid-turn; --force interrupts it \
                 and has it resume"
            )
        );
        for (line, agent) in lines[1..].iter().zip(["qa", "reviewer"]) {
            assert!(
                line.starts_with(&format!("{session}: Restarted agent '{agent}'")),
                "{line}"
            );
        }
        assert_eq!(sweep.summary(), "Restarted 2, skipped 1, failed 0");
        assert!(
            tmux::pane_processes(server.name(), &format!("{session}:implementer"))
                .unwrap()
                .iter()
                .any(|p| p.command.contains("999")),
            "a skipped agent keeps running"
        );
    }

    #[test]
    fn all_with_force_interrupts_a_mid_turn_agent_and_tells_only_it_to_resume() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project, &session, "login", "implementer");
        server.spawn_idle_fake_agent(&project, &session, "login", "reviewer");

        let scope = Scope::of(&project, "login").unwrap();
        let sweep = restart_all(&[scope], true, server.name()).unwrap().sweep();

        let lines = lines(&sweep);
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(
            lines[0].starts_with(&format!("{session}: Restarted agent 'implementer'"))
                && lines[0].ends_with("is told to resume"),
            "{lines:#?}"
        );
        assert!(!lines[1].contains("resume"), "{lines:#?}");
        assert_eq!(
            sweep.summary(),
            "Restarted 2, skipped 0, failed 0 (1 interrupted mid-turn by --force, each told to \
             resume)"
        );
        let told = |agent: &str| {
            crate::messages::list(
                &paths::messages_dir(&project),
                "login",
                agent,
                Some(crate::commands::agent_restart::RESUME_SENDER),
            )
            .unwrap()
            .len()
        };
        assert_eq!((told("implementer"), told("reviewer")), (1, 0));
    }

    #[test]
    fn global_covers_every_scope_of_every_project_and_leaves_closed_sessions_closed() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (first, first_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&first);
        let other_name = server.scope("other");
        let other = dir.path().join(&other_name);
        crate::commands::init::init(&other, &projects_dir, None, server.name()).unwrap();
        let login = tmux::session_name(&first_name, "login");
        let other_main = tmux::session_name(&other_name, "main");
        server.spawn_idle_fake_agent(&first, &login, "login", "reviewer");
        server.spawn_idle_fake_agent(&other, &other_main, "main", "implementer");
        crate::commands::feat_new::feat_new(
            &crate::commands::feat_new::FeatNewParams::with_defaults(
                &first,
                &projects_dir,
                "signup",
                server.name(),
            ),
        )
        .unwrap();
        let signup = tmux::session_name(&first_name, "signup");
        server.spawn_dead_fake_agent(&first, &signup, "signup", "implementer");
        tmux::kill_session(server.name(), &signup).unwrap();
        let first_main = tmux::session_name(&first_name, "main");
        server.spawn_dead_fake_agent(&first, &first_main, "main", "qa");
        tmux::kill_session(server.name(), &first_main).unwrap();

        let (scopes, unread) = global_scopes(&projects_dir).unwrap();
        assert!(unread.is_empty(), "{unread:?}");
        let sweep = restart_all(&scopes, false, server.name()).unwrap().sweep();

        let lines = lines(&sweep);
        assert_eq!(
            sweep.summary(),
            "Restarted 2, skipped 2, failed 0",
            "{lines:#?}"
        );
        for (session, agent) in [(&signup, "implementer"), (&first_main, "qa")] {
            assert!(
                lines.contains(&format!(
                    "{session}: Skipped agent '{agent}': its session is closed"
                )),
                "{lines:#?}"
            );
            assert!(!tmux::has_session(server.name(), session).unwrap());
        }
        for (session, agent) in [(&login, "reviewer"), (&other_main, "implementer")] {
            assert!(
                lines
                    .iter()
                    .any(|l| l.starts_with(&format!("{session}: Restarted agent '{agent}'"))),
                "{lines:#?}"
            );
        }
    }
}
