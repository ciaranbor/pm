//! The agents of a scope that are running, and what each one's window is
//! doing ([`Liveness`]).
//!
//! An agent is idle only while it waits in pm's Stop hook. A window whose
//! agent pane ([`tmux::mark_agent_pane`]) has its shell back at the prompt
//! — in the terminal's foreground, running no job — and no process of the
//! agent's harness ([`Harness::runs_as`]) is dead: the harness exited to
//! the shell and nothing will wake it. A foreground job is the harness
//! whatever it is named, so a wrapper with another name doesn't read dead,
//! and a shell's own background helpers don't read busy. Anything else is
//! busy, a harness that yielded for background work included. A pane whose
//! processes cannot be read counts as busy.
//!
//! [`Windows`] reads every pane on the server and the process table once,
//! so classifying any number of agents costs one `tmux` and one `ps` call.

use std::path::Path;

use crate::error::Result;
use crate::harness::Harness;
use crate::state::agent::{AgentEntry, AgentRegistry};
use crate::state::paths;
use crate::state::project::HarnessConfig;
use crate::state::runtime::{self, SessionPath, Waiting, WaitingKind};
use crate::tmux::{self, Pane, Process, ProcessTable};

use super::hooks_install::runs_stop_hook;

/// An active agent with a window.
pub struct RunningAgent {
    pub name: String,
    pub entry: AgentEntry,
    /// The window's tmux target.
    pub window: String,
}

/// The agents of `scope` that are running: active, with a window.
/// Advisory — state that cannot be read names no agent.
pub fn running_in_scope(
    project_root: &Path,
    project_name: &str,
    scope: &str,
    tmux_server: Option<&str>,
) -> Vec<RunningAgent> {
    let Ok(registry) = AgentRegistry::load(&paths::agents_dir(project_root), scope) else {
        return Vec::new();
    };
    let session = tmux::session_name(project_name, scope);
    registry
        .agents
        .into_iter()
        .filter(|(_, entry)| entry.active)
        .filter_map(|(name, entry)| {
            let window = tmux::find_window(tmux_server, &session, &entry.window_name).ok()??;
            Some(RunningAgent {
                name,
                entry,
                window,
            })
        })
        .collect()
}

/// What an agent's window is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// Waiting in pm's Stop hook, between turns.
    Idle,
    /// Mid-turn, or running background work.
    Busy,
    /// No harness running: it exited to the pane's shell.
    Dead,
}

/// Classify a window by the processes of its pane, the pane's own first;
/// `None`, processes that could not be read, counts as busy.
pub fn liveness(
    processes: Option<&[Process]>,
    harness: Harness,
    config: &HarnessConfig,
) -> Liveness {
    let Some(processes) = processes else {
        return Liveness::Busy;
    };
    let at_prompt = processes.first().is_some_and(|shell| shell.foreground);
    if is_idle(processes) {
        Liveness::Idle
    } else if !at_prompt
        || processes
            .iter()
            .any(|p| harness.runs_as(&p.command, config))
    {
        Liveness::Busy
    } else {
        Liveness::Dead
    }
}

/// What an agent its window reads as busy is at: its waiting marker, unless
/// its harness recorded an interrupt after it, which fires no hook. Once
/// that interrupt is claimed ([`runtime::claim_interrupt`]) the agent was
/// prompted, so it is at nothing: busy.
pub fn waiting(project_root: &Path, scope: &str, agent: &str, harness: Harness) -> Option<Waiting> {
    let marker = runtime::read_waiting(project_root, scope, agent);
    let interrupted =
        runtime::read_session_path(project_root, scope, agent, SessionPath::Transcript)
            .and_then(|transcript| harness.interrupted(&transcript))
            .map(chrono::DateTime::<chrono::Utc>::from)
            .filter(|at| marker.as_ref().is_none_or(|m| *at > m.since));
    match interrupted {
        Some(since) if runtime::interrupt_claimed(project_root, scope, agent, since) => None,
        Some(since) => Some(Waiting {
            since,
            ..Waiting::now(WaitingKind::Interrupted, None)
        }),
        None => marker,
    }
}

/// Whether an agent's pane processes show it waiting in pm's Stop hook,
/// between turns.
pub fn is_idle(processes: &[Process]) -> bool {
    processes.iter().any(|p| runs_stop_hook(&p.command))
}

/// Every window on a tmux server with the processes of its agent pane, read
/// at one moment.
pub struct Windows {
    panes: Vec<Pane>,
    /// `None` when `ps` could not be run.
    table: Option<ProcessTable>,
}

impl Windows {
    pub fn read(tmux_server: Option<&str>) -> Result<Self> {
        Ok(Self {
            panes: tmux::agent_panes(tmux_server)?,
            table: ProcessTable::read().ok(),
        })
    }

    pub fn has_session(&self, session: &str) -> bool {
        self.panes.iter().any(|p| p.session == session)
    }

    /// The window named `window_name` in `session`.
    pub fn find(&self, session: &str, window_name: &str) -> Option<&Pane> {
        self.panes
            .iter()
            .find(|p| p.session == session && p.window_name == window_name)
    }

    /// The processes of `pane`; `None` when they cannot be read.
    pub fn processes(&self, pane: &Pane) -> Option<Vec<Process>> {
        self.table.as_ref().map(|table| table.tree(pane.pid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn a_window_is_idle_in_the_hook_busy_in_its_harness_and_dead_without_one() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project, &session, "login", "busy");
        server.spawn_idle_fake_agent(&project, &session, "login", "idle");
        server.spawn_dead_fake_agent(&project, &session, "login", "dead");

        let windows = Windows::read(server.name()).unwrap();
        let config = HarnessConfig::default();
        let state = |name: &str| {
            liveness(
                windows
                    .processes(windows.find(&session, name).unwrap())
                    .as_deref(),
                Harness::ClaudeCode,
                &config,
            )
        };
        assert_eq!(state("busy"), Liveness::Busy);
        assert_eq!(state("idle"), Liveness::Idle);
        assert_eq!(state("dead"), Liveness::Dead);
    }

    #[test]
    fn a_foreground_job_of_any_name_is_busy_and_a_background_one_is_not() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let session = server.scope("jobs");
        tmux::create_session(server.name(), &session, dir.path()).unwrap();
        let state = |command: &str| {
            let window = tmux::new_window(server.name(), &session, dir.path(), None, true).unwrap();
            tmux::send_keys(server.name(), &window, command).unwrap();
            let config = HarnessConfig::default();
            let mut last = None;
            for _ in 0..250 {
                let processes = tmux::pane_processes(server.name(), &window).unwrap();
                if processes.iter().any(|p| p.command.contains("sleep")) {
                    return liveness(Some(&processes), Harness::ClaudeCode, &config);
                }
                last = Some(processes);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("{command} never started: {last:?}");
        };

        assert_eq!(state("sleep 999"), Liveness::Busy);
        assert_eq!(state("sleep 999 &"), Liveness::Dead);
    }

    #[test]
    fn a_harness_is_recognised_by_its_programs_file_name() {
        let config = HarnessConfig::default();
        let process = |command: &str| Process::new_for_test(1, command);
        let runs = |harness: Harness, command: &str| {
            liveness(Some(&[process(command)]), harness, &config) == Liveness::Busy
        };
        assert!(runs(Harness::ClaudeCode, "claude --agent implementer"));
        assert!(runs(Harness::ClaudeCode, "/opt/bin/claude --resume x"));
        assert!(runs(
            Harness::Codex,
            "node /usr/local/bin/codex --no-daemon"
        ));
        assert!(runs(Harness::OpenCode, "opencode --standalone"));
        assert!(!runs(Harness::ClaudeCode, "/bin/zsh"));
        assert!(!runs(Harness::ClaudeCode, "vim notes about claude"));
        assert!(!runs(Harness::Codex, "claude --agent implementer"));

        let mut custom = HarnessConfig::default();
        custom.opencode.binary = Some("/opt/oc/opencode-dev".to_string());
        let process = process("/opt/oc/opencode-dev --standalone");
        assert_eq!(
            liveness(
                Some(std::slice::from_ref(&process)),
                Harness::OpenCode,
                &custom
            ),
            Liveness::Busy
        );
        assert_eq!(
            liveness(Some(&[process]), Harness::OpenCode, &config),
            Liveness::Dead
        );
    }

    #[test]
    fn an_interrupt_refines_an_agent_only_when_newer_than_its_marker() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let transcript = root.join("session.jsonl");
        let interrupt = r#"{"type":"user","message":{"content":"[Request interrupted by user]"}}"#;
        std::fs::write(&transcript, format!("{interrupt}\n")).unwrap();
        let written = std::fs::metadata(&transcript).unwrap().modified().unwrap();
        runtime::write_session_path(
            root,
            "login",
            "qa",
            SessionPath::Transcript,
            Some(&transcript),
        )
        .unwrap();
        let kind = |harness| waiting(root, "login", "qa", harness).map(|w| w.kind);

        assert_eq!(kind(Harness::ClaudeCode), Some(WaitingKind::Interrupted));
        assert_eq!(kind(Harness::Codex), None);

        let mut newer = Waiting::now(WaitingKind::Prompt, None);
        newer.since = (written + std::time::Duration::from_secs(1)).into();
        runtime::write_waiting(root, "login", "qa", &newer).unwrap();
        assert_eq!(kind(Harness::ClaudeCode), Some(WaitingKind::Prompt));

        let mut older = newer.clone();
        older.since = (written - std::time::Duration::from_secs(1)).into();
        runtime::write_waiting(root, "login", "qa", &older).unwrap();
        assert_eq!(kind(Harness::ClaudeCode), Some(WaitingKind::Interrupted));
    }
}
