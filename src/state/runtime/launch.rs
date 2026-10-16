//! The **launched stamp** is a file the line a spawn types into the agent's
//! window creates before it starts the harness, so its existence says the
//! window's shell got through its startup files and ran the line, and its
//! mtime says when. Every spawn removes it before typing the line.
//!
//! The **start stamp** is a file the harness's session start writes
//! ([`hooks_session_start`](crate::commands::hooks_session_start)), every
//! harness's, a resume's included but not a compaction's, so its existence
//! says the harness got through its own startup and is up, and its mtime
//! says when; the launch check reads it
//! ([`launch_check`](crate::commands::launch_check)). Every spawn removes it
//! with the launched stamp.
//!
//! The **launch stamp** is what the agent's last spawn launched with
//! ([`launch_stamp`](crate::commands::launch_stamp)).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use super::{agent_dir, agent_file};
use crate::error::Result;
use crate::fs_utils::write_atomic;

const LAUNCHED_FILE: &str = "launched";
const STARTED_FILE: &str = "started";
const LAUNCH_PROMPT_FILE: &str = "launch-prompt";
const LAUNCH_STAMP_FILE: &str = "launch-stamp";

pub fn write_launch_stamp(
    project_root: &Path,
    scope: &str,
    agent: &str,
    stamp: &str,
) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(LAUNCH_STAMP_FILE);
    write_atomic(&file, stamp.as_bytes())
}

/// The agent's launch stamp; `None` when its spawn wrote none.
pub fn read_launch_stamp(project_root: &Path, scope: &str, agent: &str) -> Option<String> {
    std::fs::read_to_string(agent_file(project_root, scope, agent, LAUNCH_STAMP_FILE)).ok()
}

/// The launched stamp's path, its directory created and the stamp removed:
/// what a spawn's typed line creates.
pub fn reset_launched(project_root: &Path, scope: &str, agent: &str) -> Result<PathBuf> {
    let file = agent_dir(project_root, scope, agent)?.join(LAUNCHED_FILE);
    match std::fs::remove_file(&file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(file),
    }
}

/// Where the agent's launched stamp is, whether or not it exists.
pub fn launched_file(project_root: &Path, scope: &str, agent: &str) -> PathBuf {
    agent_file(project_root, scope, agent, LAUNCHED_FILE)
}

/// When the agent's window ran its spawn's typed line; `None` until it has.
pub fn launched_at(project_root: &Path, scope: &str, agent: &str) -> Option<DateTime<Utc>> {
    let file = agent_file(project_root, scope, agent, LAUNCHED_FILE);
    Some(std::fs::metadata(file).ok()?.modified().ok()?.into())
}

/// Record whether the agent's spawn launches it with a prompt, whose turn's
/// end starts its waiter, so its session's start need not
/// ([`take_launch_prompt`]).
pub fn set_launch_prompt(
    project_root: &Path,
    scope: &str,
    agent: &str,
    prompted: bool,
) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(LAUNCH_PROMPT_FILE);
    if prompted {
        std::fs::write(file, "")?;
    } else if let Err(e) = std::fs::remove_file(file)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(e.into());
    }
    Ok(())
}

/// Whether the session starting now was launched with a prompt
/// ([`set_launch_prompt`]), forgetting it: a later start in the same
/// session, a `/clear`, had none.
pub fn take_launch_prompt(project_root: &Path, scope: &str, agent: &str) -> bool {
    std::fs::remove_file(agent_file(project_root, scope, agent, LAUNCH_PROMPT_FILE)).is_ok()
}

/// Stamp the agent's harness session as started now.
pub fn mark_started(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(STARTED_FILE);
    std::fs::File::options()
        .create(true)
        .append(true)
        .open(file)?
        .set_modified(std::time::SystemTime::now())?;
    Ok(())
}

/// Remove the start stamp, as a spawn does before it launches.
pub fn reset_started(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    match std::fs::remove_file(agent_file(project_root, scope, agent, STARTED_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// When the agent's harness session last started since its spawn; `None`
/// until it has.
pub fn started_at(project_root: &Path, scope: &str, agent: &str) -> Option<DateTime<Utc>> {
    let file = agent_file(project_root, scope, agent, STARTED_FILE);
    Some(std::fs::metadata(file).ok()?.modified().ok()?.into())
}
