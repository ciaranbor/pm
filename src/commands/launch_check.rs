//! Whether the harnesses pm just launched stayed up.
//!
//! A launch is a command line typed into the window's shell, so a harness
//! that exits at once — its CLI rejecting a flag pm passed from config —
//! leaves the window open at a shell prompt, and the spawn that typed it has
//! nothing to report but success. [`confirm_all`] watches each window with the
//! snapshot's own dead-detection ([`liveness`]) until its harness has stayed
//! up for a couple of seconds, or exits.
//!
//! A window whose shell is at its prompt and has run no harness yet is
//! either still starting its shell or ran a harness that exited between two
//! looks; the two read the same, so it is judged dead only once the
//! snapshot would stop reading it as starting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::{PmError, Result};
use crate::harness::Harness;
use crate::state::agent::{AgentRegistry, AgentType};
use crate::state::paths;
use crate::state::project::{GlobalConfig, HarnessConfig, ProjectConfig, resolve_harness_config};
use crate::tmux;

use super::attention::STARTING_SECS;
use super::running_agents::{Liveness, Windows, liveness};

/// How long a harness must run before its launch counts as a success.
const STAY_UP: Duration = Duration::from_secs(2);

const POLL: Duration = Duration::from_millis(50);

/// How many of a failed window's last non-empty lines are reported.
const OUTPUT_LINES: usize = 6;

/// An agent pm just launched a harness for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub project_root: PathBuf,
    pub scope: String,
    pub agent: String,
}

/// An agent whose harness did not stay up.
#[derive(Debug)]
pub struct FailedLaunch {
    pub launch: Launch,
    /// The last lines its window showed, where the harness says why.
    pub output: String,
}

impl FailedLaunch {
    pub fn message(&self) -> String {
        let Launch { agent, scope, .. } = &self.launch;
        let mut message = format!(
            "agent '{agent}': its harness exited at launch (fix the cause, then \
             `pm agent restart {agent} --scope {scope}`)"
        );
        if !self.output.is_empty() {
            message.push_str("; its window shows:\n");
            message.push_str(&self.output);
        }
        message
    }
}

/// [`confirm_all`] for `agents` of one scope.
pub fn confirm(
    project_root: &Path,
    scope: &str,
    agents: &[String],
    tmux_server: Option<&str>,
) -> Vec<FailedLaunch> {
    let launches: Vec<Launch> = agents
        .iter()
        .map(|agent| Launch {
            project_root: project_root.to_path_buf(),
            scope: scope.to_string(),
            agent: agent.clone(),
        })
        .collect();
    confirm_all(&launches, tmux_server)
}

/// [`confirm`] for one agent, as the error its spawn fails with.
pub fn check(
    project_root: &Path,
    scope: &str,
    agent: &str,
    tmux_server: Option<&str>,
) -> Result<()> {
    match confirm(project_root, scope, &[agent.to_string()], tmux_server).first() {
        Some(failure) => Err(PmError::Agent(failure.message())),
        None => Ok(()),
    }
}

/// [`confirm`] for every active agent registered in `scope`, as after a
/// feature's whole team was launched with it.
pub fn confirm_scope(
    project_root: &Path,
    scope: &str,
    tmux_server: Option<&str>,
) -> Vec<FailedLaunch> {
    let Ok(registry) = AgentRegistry::load(&paths::agents_dir(project_root), scope) else {
        return Vec::new();
    };
    let agents: Vec<String> = registry
        .agents
        .iter()
        .filter(|(_, e)| e.agent_type == AgentType::Agent && e.active)
        .map(|(name, _)| name.clone())
        .collect();
    confirm(project_root, scope, &agents, tmux_server)
}

/// Wait until the harness of each of `launches`, made just now, has stayed
/// up or exited; the ones that exited. All are watched together, so the
/// wait does not grow with their number. Advisory: an agent pm cannot find
/// (no readable config or registry entry, no window) is not watched, and
/// windows that cannot be read report none, so a launch that worked is
/// never failed by the check.
pub fn confirm_all(launches: &[Launch], tmux_server: Option<&str>) -> Vec<FailedLaunch> {
    let start_deadline = Duration::from_secs(STARTING_SECS as u64);
    confirm_within(launches, tmux_server, start_deadline)
}

/// [`confirm_all`], judging a window that never ran a harness dead after
/// `start_deadline`.
fn confirm_within(
    launches: &[Launch],
    tmux_server: Option<&str>,
    start_deadline: Duration,
) -> Vec<FailedLaunch> {
    watch(launches, tmux_server, start_deadline).unwrap_or_default()
}

/// What watching one launch needs to know.
struct Watched<'a> {
    launch: &'a Launch,
    session: String,
    window_name: String,
    harness: Harness,
    harness_config: HarnessConfig,
    up_since: Option<Instant>,
}

fn watch(
    launches: &[Launch],
    tmux_server: Option<&str>,
    start_deadline: Duration,
) -> Result<Vec<FailedLaunch>> {
    if launches.is_empty() {
        return Ok(Vec::new());
    }
    let global = GlobalConfig::load_or_default().harness;
    let mut projects: HashMap<&Path, Option<(String, HarnessConfig)>> = HashMap::new();
    let mut registries: HashMap<(&Path, &str), Option<AgentRegistry>> = HashMap::new();
    let mut pending = Vec::new();
    for launch in launches {
        let root = launch.project_root.as_path();
        let project = projects.entry(root).or_insert_with(|| {
            let config = ProjectConfig::load(&paths::pm_dir(root)).ok()?;
            let harness_config = resolve_harness_config(&config.harness, &global);
            Some((config.project.name, harness_config))
        });
        let registry = registries
            .entry((root, launch.scope.as_str()))
            .or_insert_with(|| AgentRegistry::load(&paths::agents_dir(root), &launch.scope).ok());
        let (Some((project_name, harness_config)), Some(entry)) = (
            project.as_ref(),
            registry.as_ref().and_then(|r| r.get(&launch.agent)),
        ) else {
            continue;
        };
        pending.push(Watched {
            launch,
            session: tmux::session_name(project_name, &launch.scope),
            window_name: entry.window_name.clone(),
            harness: entry.harness,
            harness_config: harness_config.clone(),
            up_since: None,
        });
    }
    let start = Instant::now();
    let mut failed = Vec::new();
    while !pending.is_empty() {
        let windows = Windows::read(tmux_server)?;
        let now = Instant::now();
        let mut still = Vec::new();
        for mut watched in pending {
            let Some(pane) = windows.find(&watched.session, &watched.window_name) else {
                continue;
            };
            let state = liveness(
                windows.processes(pane).as_deref(),
                watched.harness,
                &watched.harness_config,
            );
            match (state, watched.up_since) {
                (Liveness::Idle, _) => {}
                (Liveness::Busy, Some(since)) if now - since >= STAY_UP => {}
                (Liveness::Busy, since) => {
                    watched.up_since = since.or(Some(now));
                    still.push(watched);
                }
                (Liveness::Dead, None) if now - start < start_deadline => still.push(watched),
                (Liveness::Dead, _) => failed.push(FailedLaunch {
                    launch: watched.launch.clone(),
                    output: last_output(tmux_server, &pane.window),
                }),
            }
        }
        pending = still;
        if !pending.is_empty() {
            std::thread::sleep(POLL);
        }
    }
    Ok(failed)
}

/// The last non-empty lines of `window`'s agent pane, indented.
fn last_output(tmux_server: Option<&str>, window: &str) -> String {
    let text = tmux::capture_pane(tmux_server, window).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(OUTPUT_LINES)..]
        .iter()
        .map(|l| format!("  {}", l.trim_end()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{TestServer, fake_claude};
    use tempfile::tempdir;

    fn launch(project: &Path, scope: &str, agent: &str) -> Launch {
        Launch {
            project_root: project.to_path_buf(),
            scope: scope.to_string(),
            agent: agent.to_string(),
        }
    }

    #[test]
    fn launches_across_scopes_are_watched_together_and_only_the_exited_ones_reported() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let login = tmux::session_name(&project_name, "login");
        let main = tmux::session_name(&project_name, "main");
        server.spawn_fake_agent(&project, &login, "login", "up");
        let quits = server.spawn_dead_fake_agent(&project, &login, "login", "quits");
        tmux::send_line(
            server.name(),
            &quits,
            &format!(
                "{} 0.5 && echo 'error: unexpected argument --bogus'",
                fake_claude().display()
            ),
        )
        .unwrap();
        server.spawn_dead_fake_agent(&project, &main, "main", "never");

        let launches = [
            launch(&project, "login", "up"),
            launch(&project, "login", "quits"),
            launch(&project, "main", "never"),
        ];
        let started = Instant::now();
        let failed = confirm_within(&launches, server.name(), Duration::from_secs(3));

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let names: Vec<(&str, &str)> = failed
            .iter()
            .map(|f| (f.launch.scope.as_str(), f.launch.agent.as_str()))
            .collect();
        assert_eq!(names, [("login", "quits"), ("main", "never")]);
        assert!(
            failed[0]
                .output
                .contains("error: unexpected argument --bogus"),
            "{}",
            failed[0].output
        );
    }
}
