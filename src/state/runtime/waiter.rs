//! The **waiter** file names the process of pm's Stop hook that waits on
//! the agent's inbox: each one takes it over as it starts, so the newest
//! wins and an older one, finding another named, ends. Idle needs it alive
//! ([`running_agents`](crate::commands::running_agents)): a marker whose
//! waiter is gone leaves the agent at its prompt with nothing to wake it.
//!
//! The **breaker** counts the waiter's wakes in a row that found the inbox
//! unread since the last one; once it trips, the **stopped-loop** file
//! says why, and no waiter wakes the agent again until a spawn forgets both.
//!
//! The **Stop hook log** records each Stop hook's path and why it took it
//! — the payload's background work included — for diagnosing an agent
//! left waiting. Best-effort, and capped.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{agent_dir, agent_file, registered};
use crate::error::Result;
use crate::fs_utils::write_atomic;

const WAITER_FILE: &str = "waiter";
const WOKEN_THROUGH_FILE: &str = "woken-through";
const BREAKER_FILE: &str = "breaker.json";
const TRIPPED_FILE: &str = "loop-tripped";
const STOP_LOG: &str = "stop-hook.log";
/// The size past which the Stop hook's log keeps only its newer half.
const STOP_LOG_MAX: u64 = 64 * 1024;

/// Name `pid` as the agent's waiter, replacing whichever was;
/// `background` is when it began waiting on background work as well as the
/// inbox, if it does.
pub fn take_waiter(
    project_root: &Path,
    scope: &str,
    agent: &str,
    pid: u32,
    background: Option<DateTime<Utc>>,
) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(WAITER_FILE);
    let text = match background {
        Some(since) => format!("{pid} {}", since.to_rfc3339()),
        None => pid.to_string(),
    };
    write_atomic(&file, text.as_bytes())
}

fn waiter_file(project_root: &Path, scope: &str, agent: &str) -> Option<String> {
    std::fs::read_to_string(agent_file(project_root, scope, agent, WAITER_FILE)).ok()
}

/// The pid of the agent's waiter; `None` when none was named.
pub fn read_waiter(project_root: &Path, scope: &str, agent: &str) -> Option<u32> {
    waiter_file(project_root, scope, agent)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// When the agent's waiter began waiting on background work, if it does.
pub fn waiter_background(project_root: &Path, scope: &str, agent: &str) -> Option<DateTime<Utc>> {
    let text = waiter_file(project_root, scope, agent)?;
    let since = text.split_whitespace().nth(1)?;
    DateTime::parse_from_rfc3339(since)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Record `detail`, what the dialog the waiter woke the agent through
/// says, for the next waiter: the woken turn's prompt clears the marker.
pub fn write_woken_through(
    project_root: &Path,
    scope: &str,
    agent: &str,
    detail: &str,
) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(WOKEN_THROUGH_FILE);
    write_atomic(&file, detail.as_bytes())
}

/// Take what [`write_woken_through`] recorded, if anything, removing it.
pub fn take_woken_through(project_root: &Path, scope: &str, agent: &str) -> Option<String> {
    let file = agent_file(project_root, scope, agent, WOKEN_THROUGH_FILE);
    let detail = std::fs::read_to_string(&file).ok();
    let _ = std::fs::remove_file(&file);
    detail
}

/// What the waiter's breaker knew at its last wake.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breaker {
    /// Wakes in a row that read nothing since the one before.
    pub wasted: u32,
    /// How many of its messages the agent had read at the last wake.
    pub read: u32,
}

pub fn read_breaker(project_root: &Path, scope: &str, agent: &str) -> Option<Breaker> {
    let text =
        std::fs::read_to_string(agent_file(project_root, scope, agent, BREAKER_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_breaker(
    project_root: &Path,
    scope: &str,
    agent: &str,
    breaker: &Breaker,
) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(BREAKER_FILE);
    write_atomic(&file, serde_json::to_string(breaker)?.as_bytes())
}

/// Record that the agent's never-idle loop stopped itself, and why.
pub fn trip_loop(project_root: &Path, scope: &str, agent: &str, reason: &str) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(TRIPPED_FILE);
    write_atomic(&file, reason.as_bytes())
}

/// Why the agent's never-idle loop stopped itself, if it did.
pub fn loop_tripped(project_root: &Path, scope: &str, agent: &str) -> Option<String> {
    let text =
        std::fs::read_to_string(agent_file(project_root, scope, agent, TRIPPED_FILE)).ok()?;
    Some(text.trim().to_string())
}

/// Forget the breaker and a stopped loop, for a new session.
pub fn reset_loop(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    for name in [BREAKER_FILE, TRIPPED_FILE] {
        match std::fs::remove_file(agent_file(project_root, scope, agent, name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

/// Append `line` to the agent's Stop hook log, stamped with the time and
/// this process's pid. Best-effort: a log that can't be written is skipped,
/// as is one for an agent no longer [registered].
pub fn log_stop_hook(project_root: &Path, scope: &str, agent: &str, line: &str) {
    if !registered(project_root, scope, agent) {
        return;
    }
    let Ok(dir) = agent_dir(project_root, scope, agent) else {
        return;
    };
    let file = dir.join(STOP_LOG);
    if std::fs::metadata(&file).is_ok_and(|m| m.len() > STOP_LOG_MAX)
        && let Ok(text) = std::fs::read_to_string(&file)
    {
        let bytes = text.as_bytes();
        let half = bytes.len() / 2;
        let start = bytes[half..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| half + i + 1);
        let _ = write_atomic(&file, &bytes[start..]);
    }
    let line = format!(
        "{} [{}] {line}\n",
        Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        std::process::id()
    );
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}
