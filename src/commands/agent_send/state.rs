//! What a send tells its sender about the recipient: the state the message
//! found it in, as `pm status` reads it, and so when it will be read.
//! Read before the message is queued, since an idle recipient's waiter may
//! wake it at once.

use std::path::Path;

use crate::commands::attention::{self, AgentSnapshot, AgentState};

/// The recipient's state in the scope's snapshot; `None` when it can't be
/// read, which only costs the sender this line.
pub(super) fn read(
    project_root: &Path,
    scope: &str,
    agent: &str,
    tmux_server: Option<&str>,
) -> Option<AgentSnapshot> {
    attention::scope_agents(project_root, scope, tmux_server)
        .ok()?
        .into_iter()
        .find(|a| a.name == agent)
}

/// The line saying what becomes of the message `agent` was sent in state
/// `found`, when no heal or re-arm acted on it.
pub(super) fn describe(agent: &str, found: &AgentSnapshot) -> String {
    let detail = || {
        found
            .waiting
            .as_ref()
            .map(|w| format!(" ({})", crate::harness::one_line(&w.detail)))
            .unwrap_or_default()
    };
    match found.state {
        AgentState::Idle => format!("'{agent}' is idle: pm wakes it to read this now"),
        AgentState::Busy => format!("'{agent}' is busy: it reads this when its turn ends"),
        AgentState::Background => format!(
            "'{agent}' is waiting on background work{}: pm wakes it to read this now",
            detail()
        ),
        AgentState::Asking => format!(
            "'{agent}' is asking the user{}: it reads this once that is answered",
            detail()
        ),
        AgentState::Unarmed => format!(
            "'{agent}' is unarmed{}: nothing wakes it to read this until it is prompted",
            detail()
        ),
        AgentState::Dead | AgentState::Stopped | AgentState::Closed => format!(
            "'{agent}' is {}: it reads this when it next runs",
            found.state
        ),
    }
}
