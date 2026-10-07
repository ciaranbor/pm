//! Whether the harnesses pm just launched came up.
//!
//! A launch is a command line typed into the window's shell, so a harness
//! that exits at once — its CLI rejecting a flag pm passed from config —
//! leaves the window open at a shell prompt, and one blocked in its own
//! startup — on a keychain that does not answer, a login or trust screen —
//! sits in the window without ever starting its session; either way the
//! spawn that typed it has nothing to report but success. [`confirm_all`]
//! watches each window until its harness process ([`Harness::runs_as`]) is
//! running and its session has started (the start stamp,
//! [`runtime::started_at`]), or the window reads dead ([`liveness`]) after
//! it ran: that launch exited. A harness that runs for [`UP_WITHIN`] without
//! its session starting has not come up; it is left running, since it may
//! yet, and what its window shows says why.
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

/// How long a harness may run before its session starts, counted from
/// when its process is first seen, so a slow shell does not use it up.
pub const UP_WITHIN: Duration = Duration::from_secs(20);

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

/// An agent whose harness exited or did not come up.
#[derive(Debug)]
pub struct FailedLaunch {
    pub launch: Launch,
    pub failure: Failure,
    /// The last lines its window showed, where the harness says why; empty
    /// when it drew nothing.
    pub output: String,
}

/// How a launch failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The harness exited: the window is back at its shell.
    Exited,
    /// The harness is running, but its session did not start within
    /// `after`.
    NotUp { harness: Harness, after: Duration },
}

impl FailedLaunch {
    /// Whether the harness was left running without coming up.
    pub fn not_up(&self) -> bool {
        matches!(self.failure, Failure::NotUp { .. })
    }

    pub fn message(&self) -> String {
        let Launch { agent, scope, .. } = &self.launch;
        let restart = format!("`pm agent restart {agent} --scope {scope}`");
        let mut message = match self.failure {
            Failure::Exited => format!(
                "agent '{agent}': its harness exited at launch (fix the cause, then {restart})"
            ),
            Failure::NotUp { harness, after } => {
                let mut message = format!(
                    "agent '{agent}': its {harness} harness started but has not come up after \
                     {after:?}"
                );
                if self.output.is_empty() {
                    message.push_str(&format!(
                        ": it has drawn nothing, so it is blocked before its startup{}",
                        blocked_hint(harness)
                    ));
                }
                message.push_str(&format!(
                    "; it is left running and may come up by itself (if not, {restart})"
                ));
                message
            }
        };
        if !self.output.is_empty() {
            message.push_str("; its window shows:\n");
            message.push_str(&self.output);
        }
        message
    }
}

/// What most often blocks `harness` when it draws nothing.
fn blocked_hint(harness: Harness) -> String {
    if cfg!(target_os = "macos") && harness.reads_keychain() {
        format!(
            " (on macOS, often a login keychain that isn't answering: `{}`)",
            crate::keychain::COMMAND
        )
    } else {
        String::new()
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

/// Wait until the harness of each of `launches`, made just now, has come
/// up, exited, or run [`UP_WITHIN`] without coming up; the ones that did
/// not come up. All are watched together, so the wait does not grow with
/// their number. Advisory: an agent pm cannot find (no readable config or
/// registry entry, no window) is not watched, and windows that cannot be
/// read report none, so a launch that worked is never failed by the check.
pub fn confirm_all(launches: &[Launch], tmux_server: Option<&str>) -> Vec<FailedLaunch> {
    confirm_within(launches, tmux_server, START_WITHIN, UP_WITHIN)
}

/// [`confirm_all`], with `start_deadline` in place of [`START_WITHIN`] and
/// `up_deadline` of [`UP_WITHIN`].
pub(crate) fn confirm_within(
    launches: &[Launch],
    tmux_server: Option<&str>,
    start_deadline: Duration,
    up_deadline: Duration,
) -> Vec<FailedLaunch> {
    watch(launches, tmux_server, start_deadline, up_deadline).unwrap_or_default()
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
    up_deadline: Duration,
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
            let Launch {
                project_root,
                scope,
                agent: name,
            } = watched.launch;
            let started = runtime::started_at(project_root, scope, name).is_some();
            match watched.up_since {
                _ if started && !dead && (running || watched.up_since.is_some()) => {}
                Some(since) if !dead && now - since < up_deadline => still.push(watched),
                Some(_) if !dead => {
                    let launched = runtime::launched_file(project_root, scope, name);
                    failed.push(FailedLaunch {
                        launch: watched.launch.clone(),
                        failure: Failure::NotUp {
                            harness: watched.harness,
                            after: up_deadline,
                        },
                        output: drawn(
                            tmux_server,
                            &pane.window,
                            &launched,
                            processes.first().map(|shell| shell.command.as_str()),
                        ),
                    });
                }
                None if running => {
                    watched.up_since = Some(now);
                    still.push(watched);
                }
                None if !settled && now - start < start_deadline => still.push(watched),
                None if !dead => {}
                _ => failed.push(FailedLaunch {
                    launch: watched.launch.clone(),
                    failure: Failure::Exited,
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
    tail(&non_empty(&text))
}

/// [`last_output`], from after the line the spawn typed, which creates
/// `launched`: empty when the harness has drawn nothing below it. Lines
/// the pane's `shell` printed itself, prefixed with its name, are left out:
/// they are its diagnostics from running the line (bash on macOS reports a
/// lost `setpgid` race under load), not the harness's screen.
fn drawn(tmux_server: Option<&str>, window: &str, launched: &Path, shell: Option<&str>) -> String {
    let text = tmux::capture_pane(tmux_server, window).unwrap_or_default();
    let lines = non_empty(&text);
    let launched = launched.to_string_lossy();
    let from = lines
        .iter()
        .rposition(|l| l.contains(launched.as_ref()))
        .map_or(0, |typed| typed + 1);
    let drawn: Vec<&str> = lines[from..]
        .iter()
        .filter(|l| !shell.is_some_and(|shell| shell_says(shell, l)))
        .copied()
        .collect();
    tail(&drawn)
}

/// Whether `line` is a diagnostic of the shell running as `command`: its
/// program's name, then `: `. A login shell's leading `-` is optional on
/// both, since bash keeps it in what it prints and zsh drops it.
fn shell_says(command: &str, line: &str) -> bool {
    let program = command.split(' ').next().unwrap_or(command);
    let name = program.rsplit('/').next().unwrap_or(program);
    line.trim_start_matches('-')
        .strip_prefix(name.trim_start_matches('-'))
        .is_some_and(|rest| rest.starts_with(": "))
}

fn non_empty(text: &str) -> Vec<&str> {
    text.lines().filter(|l| !l.trim().is_empty()).collect()
}

fn tail(lines: &[&str]) -> String {
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

    /// A script named `claude` in a directory of its own, so a pane running
    /// it runs the harness, that runs `body` and then execs [`fake_claude`].
    fn fake_claude_doing(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join(name).join("claude");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(
            &bin,
            format!("#!/bin/sh\n{body}\nexec {} 999\n", fake_claude().display()),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn a_shell_diagnostic_is_recognised_with_or_without_its_login_dash() {
        let setpgid = "child setpgid (1 to 1): Operation not permitted";
        for (command, line) in [
            ("-bash", format!("-bash: {setpgid}")),
            ("/bin/bash -l", format!("bash: {setpgid}")),
            ("-zsh", "zsh: command not found: claude".to_string()),
            ("/bin/sh", format!("sh: {setpgid}")),
        ] {
            assert!(shell_says(command, &line), "{command}: {line}");
        }
        assert!(!shell_says("-bash", "bashful: hello"));
        assert!(!shell_says(
            "/bin/sh",
            "Do you trust the files in this folder?"
        ));
    }

    #[test]
    fn launches_across_scopes_are_watched_together_and_only_the_failed_ones_reported() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let login = tmux::session_name(&project_name, "login");
        let main = tmux::session_name(&project_name, "main");
        server.spawn_fake_agent(&project, &login, "login", "up");
        runtime::mark_started(&project, "login", "up").unwrap();
        // What a spawn types: the launch stamp, then the harness.
        let type_launch = |scope: &str, agent: &str, window: &str, line: String| {
            runtime::reset_started(&project, scope, agent).unwrap();
            let stamp = runtime::reset_launched(&project, scope, agent).unwrap();
            let stamp = tmux::shell_quote(&stamp.to_string_lossy());
            tmux::send_line(server.name(), window, &format!("touch {stamp} && {line}")).unwrap();
        };
        let started = |scope: &str, agent: &str| {
            let dir = runtime::agent_dir(&project, scope, agent).unwrap();
            tmux::shell_quote(&dir.join("started").to_string_lossy())
        };
        // A harness whose session starts a moment after it does.
        let comes_up = fake_claude_doing(dir.path(), "comes-up", r#"(sleep 0.3; touch "$1") &"#);
        let talks = fake_claude_doing(
            dir.path(),
            "talks",
            "echo 'Do you trust the files in this folder?'",
        );
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
        let line = format!("{} {}", comes_up.display(), started("main", "slow"));
        type_launch("main", "slow", &slow, line);
        // A startup file waiting inside the shell, asleep with nothing in
        // the foreground, before the launch line runs.
        let waits = server.spawn_dead_fake_agent(&project, &main, "main", "waits");
        tmux::send_line(server.name(), &waits, "sleep 1.5 & wait").unwrap();
        let line = format!("{} {}", comes_up.display(), started("main", "waits"));
        type_launch("main", "waits", &waits, line);
        // Blocked before its startup, drawing nothing; what its shell
        // printed running the line is not the harness's.
        let silent = server.spawn_dead_fake_agent(&project, &main, "main", "silent");
        type_launch(
            "main",
            "silent",
            &silent,
            format!("pm-test-no-such-command; {fake} 999"),
        );
        // Held on a screen of its own before its session starts.
        let asks = server.spawn_dead_fake_agent(&project, &main, "main", "asks");
        type_launch("main", "asks", &asks, talks.display().to_string());

        let launches = [
            launch(&project, "login", "up"),
            launch(&project, "login", "quits"),
            launch(&project, "main", "instant"),
            launch(&project, "main", "slow"),
            launch(&project, "main", "waits"),
            launch(&project, "main", "silent"),
            launch(&project, "main", "asks"),
        ];
        let asked = Instant::now();
        let failed = confirm_within(
            &launches,
            server.name(),
            Duration::from_secs(30),
            Duration::from_secs(4),
        );

        assert!(
            asked.elapsed() < Duration::from_secs(12),
            "{:?}",
            asked.elapsed()
        );
        let names: Vec<(&str, &str, bool)> = failed
            .iter()
            .map(|f| (f.launch.scope.as_str(), f.launch.agent.as_str(), f.not_up()))
            .collect();
        assert_eq!(
            names,
            [
                ("login", "quits", false),
                ("main", "instant", false),
                ("main", "silent", true),
                ("main", "asks", true),
            ]
        );
        assert!(
            failed[0]
                .output
                .contains("error: unexpected argument --bogus"),
            "{}",
            failed[0].output
        );
        let silent = failed[2].message();
        assert!(
            silent.contains("has not come up after 4s: it has drawn nothing")
                && silent.ends_with(
                    "it is left running and may come up by itself (if not, \
                     `pm agent restart silent --scope main`)"
                ),
            "{silent}"
        );
        assert!(
            failed[3]
                .message()
                .ends_with("its window shows:\n  Do you trust the files in this folder?"),
            "{}",
            failed[3].message()
        );
        for agent in ["silent", "asks"] {
            assert!(
                tmux::pane_processes(server.name(), &format!("{main}:{agent}"))
                    .unwrap()
                    .iter()
                    .any(|p| p.command.contains("999")),
                "{agent} is left running"
            );
        }
    }
}
