//! The **end stamp**: the harness process whose session ended by its own
//! session-end hook ([`hooks_session_end`](crate::commands::hooks_session_end)),
//! so its window reads dead while that process is still shutting down
//! ([`running_agents`](crate::commands::running_agents)). It names the
//! process, so a late end from a session a restart already replaced does
//! not mark the new one. A spawn, a session's start and a prompt remove it.

use std::path::Path;

use super::{agent_dir, agent_file};
use crate::error::Result;
use crate::fs_utils::write_atomic;

const ENDED_FILE: &str = "ended";

/// Stamp the session of harness process `pid` as ended.
pub fn mark_ended(project_root: &Path, scope: &str, agent: &str, pid: u32) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(ENDED_FILE);
    write_atomic(&file, pid.to_string().as_bytes())
}

/// The harness process whose session ended, if one did since the last
/// start.
pub fn read_ended(project_root: &Path, scope: &str, agent: &str) -> Option<u32> {
    std::fs::read_to_string(agent_file(project_root, scope, agent, ENDED_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub fn clear_ended(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    match std::fs::remove_file(agent_file(project_root, scope, agent, ENDED_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}
