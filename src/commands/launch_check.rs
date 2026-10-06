//! Whether the harnesses pm just launched stayed up.
//!
//! A launch is a command line typed into the window's shell, so a harness
//! that exits at once — its CLI rejecting a flag pm passed from config —
//! leaves the window open at a shell prompt, and the spawn that typed it has
//! nothing to report but success. [`confirm_all`] watches each window until
//! its harness process ([`Harness::runs_as`]) has stayed up for a couple of
//! seconds, or the window reads dead ([`liveness`]) after it ran.
//!
//! Until the harness process has been seen the launch is still starting,
//! whatever the window shows: a new shell runs its startup files' commands,
//! some as foreground jobs and some in itself, before it reads the typed
//! line, which under load can take seconds. A harness rejecting its flags
//! can exit in tens of milliseconds, too fast to be seen, so a launch also
//! fails once its shell has run the typed line (the line creates the
//! agent's launch stamp, [`runtime::launched_at`]) and then sat waiting for
//! input, asleep at its prompt, for a moment without the harness seen. A
//! shell asleep in a startup file (`read -t`, `wait`) before it ran the line
//! is still starting.
//! A window that settles neither way is judged by its liveness after a
//! generous deadline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::{PmError, Result};
use crate::harness::Harness;
use crate::state::agent::{AgentRegistry, AgentType};
use crate::state::paths;
use crate::state::project::{GlobalConfig, HarnessConfig, ProjectConfig, resolve_harness_config};
use crate::state::runtime;
use crate::tmux::{self, Process};

use super::running_agents::{AgentAt, Liveness, Windows, classify, liveness};

/// How long a harness must run before its launch counts as a success.
const STAY_UP: Duration = Duration::from_secs(2);

/// How long a window may go without showing its harness before its launch
/// is judged by its liveness.
pub const START_WITHIN: Duration = Duration::from_secs(30);

/// How long a shell must wait for input, its harness unseen, before the
/// harness counts as having exited too fast to be seen.
const SETTLE: Duration = Duration::from_millis(500);

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
    confirm_within(launches, tmux_server, START_WITHIN)
}

/// [`confirm_all`], with `start_deadline` in place of [`START_WITHIN`].
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
    /// When its harness process was first seen.
    up_since: Option<Instant>,
    /// Since when, before that, its shell has been waiting for input.
    reading_since: Option<Instant>,
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
            reading_since: None,
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
            let Some(processes) = windows.processes(pane) else {
                continue;
            };
            let agent = AgentAt {
                project_root: &watched.launch.project_root,
                scope: &watched.launch.scope,
                name: &watched.launch.agent,
                harness: watched.harness,
            };
            if classify(agent, Some(&processes), &watched.harness_config).0 == Liveness::Idle {
                continue;
            }
            let running = processes
                .iter()
                .any(|p| watched.harness.runs_as(&p.command, &watched.harness_config));
            let dead = liveness(Some(&processes), watched.harness, &watched.harness_config)
                == Liveness::Dead;
            let at_input = dead
                && reading_input(&processes)
                && runtime::launched_at(
                    &watched.launch.project_root,
                    &watched.launch.scope,
                    &watched.launch.agent,
                )
                .is_some();
            watched.reading_since = at_input.then(|| watched.reading_since.unwrap_or(now));
            let settled = watched
                .reading_since
                .is_some_and(|since| now - since >= SETTLE);
            match watched.up_since {
                Some(since) if !dead && now - since >= STAY_UP => {}
                Some(_) if !dead => still.push(watched),
                None if running => {
                    watched.up_since = Some(now);
                    still.push(watched);
                }
                None if !settled && now - start < start_deadline => still.push(watched),
                None if !dead => {}
                _ => failed.push(FailedLaunch {
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

/// Whether a pane's shell, the first of its `processes`, is at its prompt
/// waiting for input, so has read every line typed into it.
fn reading_input(processes: &[Process]) -> bool {
    processes.split_first().is_some_and(|(shell, rest)| {
        shell.foreground && shell.asleep && rest.iter().all(|p| !p.foreground)
    })
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
        // What a spawn types: the launch stamp, then the harness.
        let type_launch = |scope: &str, agent: &str, window: &str, line: String| {
            let stamp = runtime::reset_launched(&project, scope, agent).unwrap();
            let stamp = tmux::shell_quote(&stamp.to_string_lossy());
            tmux::send_line(server.name(), window, &format!("touch {stamp} && {line}")).unwrap();
        };
        let fake = fake_claude().display().to_string();
        let quits = server.spawn_dead_fake_agent(&project, &login, "login", "quits");
        type_launch(
            "login",
            "quits",
            &quits,
            format!("{fake} 0.5 && echo 'error: unexpected argument --bogus'"),
        );
        let instant = server.spawn_dead_fake_agent(&project, &main, "main", "instant");
        type_launch("main", "instant", &instant, format!("{fake} 0"));
        let slow = server.spawn_dead_fake_agent(&project, &main, "main", "slow");
        tmux::send_line(
            server.name(),
            &slow,
            "sleep 0.3; end=$(($(date +%s) + 2)); \
             while [ \"$(date +%s)\" -lt $end ]; do :; done",
        )
        .unwrap();
        type_launch("main", "slow", &slow, format!("{fake} 999"));
        // A startup file waiting inside the shell, asleep with nothing in
        // the foreground, before the launch line runs.
        let waits = server.spawn_dead_fake_agent(&project, &main, "main", "waits");
        tmux::send_line(server.name(), &waits, "sleep 1.5 & wait").unwrap();
        type_launch("main", "waits", &waits, format!("{fake} 999"));

        let launches = [
            launch(&project, "login", "up"),
            launch(&project, "login", "quits"),
            launch(&project, "main", "instant"),
            launch(&project, "main", "slow"),
            launch(&project, "main", "waits"),
        ];
        let started = Instant::now();
        let failed = confirm_within(&launches, server.name(), Duration::from_secs(30));

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        let names: Vec<(&str, &str)> = failed
            .iter()
            .map(|f| (f.launch.scope.as_str(), f.launch.agent.as_str()))
            .collect();
        assert_eq!(names, [("login", "quits"), ("main", "instant")]);
        assert!(
            failed[0]
                .output
                .contains("error: unexpected argument --bogus"),
            "{}",
            failed[0].output
        );
    }
}
