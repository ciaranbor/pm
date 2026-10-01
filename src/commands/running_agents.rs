//! The agents of a scope that are running, and which of them are mid-turn.
//!
//! An agent is idle only while it waits in pm's Stop hook, so an agent whose
//! pane has no process running that hook is busy: whatever the harness, and
//! including a harness that yielded for background work or a window that
//! runs no harness at all. A pane whose processes cannot be read counts as
//! busy.

use std::path::Path;

use crate::state::agent::{AgentEntry, AgentRegistry};
use crate::state::paths;
use crate::tmux::{self, Process};

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

/// Whether `processes` include the current one.
pub fn runs_this_process(processes: &[Process]) -> bool {
    processes.iter().any(|p| p.pid == std::process::id())
}

/// Whether an agent's pane processes show it waiting in pm's Stop hook,
/// between turns.
pub fn is_idle(processes: &[Process]) -> bool {
    processes.iter().any(|p| runs_stop_hook(&p.command))
}

/// The running agents of `scope` that are mid-turn, other than the one this
/// process runs in.
pub fn busy_in_scope(
    project_root: &Path,
    project_name: &str,
    scope: &str,
    tmux_server: Option<&str>,
) -> Vec<String> {
    running_in_scope(project_root, project_name, scope, tmux_server)
        .into_iter()
        .filter(|agent| {
            let processes = tmux::pane_processes(tmux_server, &agent.window).unwrap_or_default();
            !runs_this_process(&processes) && !is_idle(&processes)
        })
        .map(|agent| agent.name)
        .collect()
}
