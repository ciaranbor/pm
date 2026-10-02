//! Whether the harnesses pm just launched stayed up.
//!
//! A launch is a command line typed into the window's shell, so a harness
//! that exits at once — its CLI rejecting a flag pm passed from config —
//! leaves the window open at a shell prompt, and the spawn that typed it has
//! nothing to report but success. [`confirm`] watches each window with the
//! snapshot's own dead-detection ([`liveness`]) until its harness has stayed
//! up for a couple of seconds, or exits.
//!
//! A window whose shell is at its prompt and has run no harness yet is
//! either still starting its shell or ran a harness that exited between two
//! looks; the two read the same, so it is judged dead only once the
//! snapshot would stop reading it as starting.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::{PmError, Result};
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig, resolve_harness_config};
use crate::tmux;

use super::attention::STARTING_SECS;
use super::running_agents::{Liveness, Windows, liveness};

/// How long a harness must run before its launch counts as a success.
const STAY_UP: Duration = Duration::from_secs(2);

const POLL: Duration = Duration::from_millis(50);

/// How many of a failed window's last non-empty lines are reported.
const OUTPUT_LINES: usize = 6;

/// An agent whose harness did not stay up.
#[derive(Debug)]
pub struct FailedLaunch {
    pub agent: String,
    /// The last lines its window showed, where the harness says why.
    pub output: String,
}

impl FailedLaunch {
    pub fn message(&self, scope: &str) -> String {
        let agent = &self.agent;
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

/// Wait until the harness of each of `agents`, launched in `scope` just
/// now, has stayed up or exited; the ones that exited. Advisory: an agent pm
/// cannot find (no registry entry, no window) is not watched, and state or
/// windows that cannot be read report none, so a launch that worked is
/// never failed by the check.
pub fn confirm(
    project_root: &Path,
    scope: &str,
    agents: &[String],
    tmux_server: Option<&str>,
) -> Vec<FailedLaunch> {
    let start_deadline = Duration::from_secs(STARTING_SECS as u64);
    confirm_within(project_root, scope, agents, tmux_server, start_deadline)
}

/// [`confirm`] for one agent, as the error its spawn fails with.
pub fn check(
    project_root: &Path,
    scope: &str,
    agent: &str,
    tmux_server: Option<&str>,
) -> Result<()> {
    match confirm(project_root, scope, &[agent.to_string()], tmux_server).first() {
        Some(failure) => Err(PmError::Agent(failure.message(scope))),
        None => Ok(()),
    }
}

/// [`confirm`], judging a window that never ran a harness dead after
/// `start_deadline`.
fn confirm_within(
    project_root: &Path,
    scope: &str,
    agents: &[String],
    tmux_server: Option<&str>,
    start_deadline: Duration,
) -> Vec<FailedLaunch> {
    watch(project_root, scope, agents, tmux_server, start_deadline).unwrap_or_default()
}

fn watch(
    project_root: &Path,
    scope: &str,
    agents: &[String],
    tmux_server: Option<&str>,
    start_deadline: Duration,
) -> Result<Vec<FailedLaunch>> {
    if agents.is_empty() {
        return Ok(Vec::new());
    }
    let config = ProjectConfig::load(&paths::pm_dir(project_root))?;
    let harness_config =
        resolve_harness_config(&config.harness, &GlobalConfig::load_or_default().harness);
    let registry = AgentRegistry::load(&paths::agents_dir(project_root), scope)?;
    let session = tmux::session_name(&config.project.name, scope);
    let mut pending: Vec<(&String, Option<Instant>)> = agents
        .iter()
        .filter(|name| registry.get(name).is_some())
        .map(|name| (name, None))
        .collect();
    let start = Instant::now();
    let mut failed = Vec::new();
    while !pending.is_empty() {
        let windows = Windows::read(tmux_server)?;
        let now = Instant::now();
        let mut still = Vec::new();
        for (name, up_since) in pending {
            let entry = registry.get(name).expect("filtered on presence");
            let Some(pane) = windows.find(&session, &entry.window_name) else {
                continue;
            };
            let state = liveness(
                windows.processes(pane).as_deref(),
                entry.harness,
                &harness_config,
            );
            match (state, up_since) {
                (Liveness::Idle, _) => {}
                (Liveness::Busy, Some(since)) if now - since >= STAY_UP => {}
                (Liveness::Busy, since) => still.push((name, since.or(Some(now)))),
                (Liveness::Dead, None) if now - start < start_deadline => {
                    still.push((name, None));
                }
                (Liveness::Dead, _) => failed.push(FailedLaunch {
                    agent: name.clone(),
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

    #[test]
    fn a_harness_that_exits_at_launch_is_reported_with_its_output_and_one_that_stays_up_is_not() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project, &session, "login", "up");
        let quits = server.spawn_dead_fake_agent(&project, &session, "login", "quits");
        tmux::send_line(
            server.name(),
            &quits,
            &format!(
                "{} 0.5 && echo 'error: unexpected argument --bogus'",
                fake_claude().display()
            ),
        )
        .unwrap();
        server.spawn_dead_fake_agent(&project, &session, "login", "never");

        let agents = ["up", "quits", "never"].map(String::from);
        let failed = confirm_within(
            &project,
            "login",
            &agents,
            server.name(),
            Duration::from_secs(3),
        );

        let names: Vec<&str> = failed.iter().map(|f| f.agent.as_str()).collect();
        assert_eq!(names, ["quits", "never"]);
        assert!(
            failed[0]
                .output
                .contains("error: unexpected argument --bogus"),
            "{}",
            failed[0].output
        );
    }
}
