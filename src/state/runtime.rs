//! Per-agent runtime files: what a spawn hands its harness (the composed
//! prompt, opencode's config) and what the harness leaves for pm (opencode's
//! stopped-loop marker, the waiting marker, the activity stamp).
//!
//! The **waiting marker** says what an agent between turns, or held up in
//! one, is at: idle (its turn ended and pm's waiter waits on its inbox), a
//! dialog waiting on the user, its prompt with nothing to wake it, or
//! background work that will. pm's hooks write and clear it, and every
//! spawn replaces it. An exited harness says more, so a stale marker never
//! hides it.
//!
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
//!
//! The **activity stamp** is a file whose mtime is the agent's last sign of
//! life: every pm hook invocation touches it.
//!
//! The **launch stamp** is a file the line a spawn types into the agent's
//! window creates before it starts the harness, so its existence says the
//! window's shell got through its startup files and ran the line, and its
//! mtime says when. Every spawn removes it before typing the line.
//!
//! The **start stamp** is a file the harness's session start writes
//! ([`hooks_session_start`](crate::commands::hooks_session_start)), every
//! harness's, a resume's included, so its existence says the harness got
//! through its own startup and is up, and its mtime says when; the launch
//! check reads it ([`launch_check`](crate::commands::launch_check)). Every
//! spawn removes it with the launch stamp.
//!
//! The **session paths** are what the current session reported at its
//! start: its transcript, for a harness that records an interrupt or a
//! failed turn only there
//! ([`Harness::turn_ended`](crate::harness::Harness::turn_ended)), and the
//! config dir its environment named, where its input settings are
//! read ([`Harness::config_dir_env`](crate::harness::Harness::config_dir_env)).
//! Every spawn forgets them until the new session starts. A turn's end read
//! from the transcript has no marker to remove, so the one who acts on it
//! claims it with a stamp instead, after which it no longer counts.
//!
//! The **dialog record** is a dialog that can be answered remotely
//! ([`DialogRecord`]).
//!
//! The **launch stamp** is what the agent's last spawn launched with
//! ([`launch_stamp`](crate::commands::launch_stamp)).
//!
//! The **restart-at-idle** file marks a stale agent a restart sweep passed
//! over as busy, for its waiter to restart at its next idle
//! ([`restart_at_idle`](crate::commands::restart_at_idle)). Every spawn
//! removes it.
//!
//! They live in `<project>/.pm/runtime/<scope>/<agent>/` and last as long
//! as the agent's registry entry. Every spawn rewrites what it hands the
//! harness, so a deleted file is restored by the next spawn. The directory
//! ignores itself, keeping machine-specific files out of the `.pm/` state
//! repo.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::fs_utils::write_atomic;
use crate::state::paths;

mod dialog;
pub use dialog::*;

const WAITING_FILE: &str = "waiting.json";
const ACTIVITY_FILE: &str = "activity";
const LAUNCHED_FILE: &str = "launched";
const STARTED_FILE: &str = "started";
const TURN_END_CLAIM: &str = "turn-end-claimed-";
const WAITER_FILE: &str = "waiter";
const BREAKER_FILE: &str = "breaker.json";
const TRIPPED_FILE: &str = "loop-tripped";
const STOP_LOG: &str = "stop-hook.log";
const LAUNCH_STAMP_FILE: &str = "launch-stamp";
const RESTART_AT_IDLE_FILE: &str = "restart-at-idle";
/// The size past which the Stop hook's log keeps only its newer half.
const STOP_LOG_MAX: u64 = 64 * 1024;

fn root(project_root: &Path) -> PathBuf {
    paths::pm_dir(project_root).join("runtime")
}

fn scope_dir(project_root: &Path, scope: &str) -> PathBuf {
    root(project_root).join(scope)
}

/// The agent's runtime dir, created if missing.
pub fn agent_dir(project_root: &Path, scope: &str, agent: &str) -> Result<PathBuf> {
    let dir = scope_dir(project_root, scope).join(agent);
    std::fs::create_dir_all(&dir)?;
    let ignore = root(project_root).join(".gitignore");
    if !ignore.exists() {
        std::fs::write(ignore, "*\n")?;
    }
    Ok(dir)
}

/// What an agent between turns, or held up in one, is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WaitingKind {
    /// Its turn ended and pm's waiter waits on its inbox; unarmed once the
    /// waiter is gone.
    Idle,
    /// A question to the user.
    Question,
    /// A tool-permission prompt.
    Permission,
    /// A plan waiting for approval.
    Plan,
    /// Any other dialog the harness shows.
    Dialog,
    /// Set at spawn, cleared when the session starts: a dialog that comes
    /// before the session (folder trust, hook trust, login).
    Startup,
    /// The user interrupted the turn.
    Interrupted,
    /// The Stop hook was killed, or failed.
    HookEnded,
    /// An API error ended the turn.
    Error,
    /// The harness reports it is at its prompt.
    Prompt,
    /// The never-idle loop stopped itself.
    Tripped,
    /// The turn ended for background work, whose completion wakes it.
    Background,
}

/// Which state a [`WaitingKind`] puts an agent in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingClass {
    /// A dialog waits on the user.
    Asking,
    /// At its prompt; a message won't wake it.
    Unarmed,
    /// Background work will wake it.
    Background,
}

impl WaitingKind {
    pub fn class(self) -> WaitingClass {
        match self {
            Self::Question | Self::Permission | Self::Plan | Self::Dialog | Self::Startup => {
                WaitingClass::Asking
            }
            Self::Idle
            | Self::Interrupted
            | Self::HookEnded
            | Self::Error
            | Self::Prompt
            | Self::Tripped => WaitingClass::Unarmed,
            Self::Background => WaitingClass::Background,
        }
    }

    /// What the kind says when the marker carries no detail.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "its waiter is gone",
            Self::Question => "question",
            Self::Permission => "permission prompt",
            Self::Plan => "plan approval",
            Self::Dialog => "dialog",
            Self::Startup => "startup dialog",
            Self::Interrupted => "interrupted",
            Self::HookEnded => "Stop hook ended",
            Self::Error => "turn failed",
            Self::Prompt => "at its prompt",
            Self::Tripped => "never-idle loop stopped",
            Self::Background => "background work",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    pub kind: WaitingKind,
    /// What the harness said about it: the question, the command.
    pub detail: Option<String>,
    pub since: DateTime<Utc>,
    /// The harness's id for the subagent whose dialog it is; `None` for the
    /// agent's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    /// Whether the agent's turn has ended: the waiter wrote the marker, or
    /// a dialog's replaced one it wrote. A subagent's dialog closing then
    /// puts back the waiter's marker, not a busy agent's none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub between_turns: bool,
    /// For a turn end read from the transcript, the id of the entry that
    /// records it, which a claim on it names ([`claim_turn_end`]).
    #[serde(skip)]
    pub entry: Option<String>,
}

impl Waiting {
    pub fn now(kind: WaitingKind, detail: Option<String>) -> Self {
        Self {
            kind,
            detail: detail.filter(|d| !d.trim().is_empty()),
            since: Utc::now(),
            subagent: None,
            between_turns: false,
            entry: None,
        }
    }

    /// The detail, else the kind's label.
    pub fn describe(&self) -> String {
        self.detail
            .clone()
            .unwrap_or_else(|| self.kind.label().to_string())
    }
}

fn agent_file(project_root: &Path, scope: &str, agent: &str, name: &str) -> PathBuf {
    scope_dir(project_root, scope).join(agent).join(name)
}

pub fn write_waiting(
    project_root: &Path,
    scope: &str,
    agent: &str,
    waiting: &Waiting,
) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(WAITING_FILE);
    write_atomic(&file, serde_json::to_string(waiting)?.as_bytes())
}

/// The agent's marker; `None` when it has none or it can't be read.
pub fn read_waiting(project_root: &Path, scope: &str, agent: &str) -> Option<Waiting> {
    let text =
        std::fs::read_to_string(agent_file(project_root, scope, agent, WAITING_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove the agent's marker. Returns whether it had one.
pub fn clear_waiting(project_root: &Path, scope: &str, agent: &str) -> Result<bool> {
    match std::fs::remove_file(agent_file(project_root, scope, agent, WAITING_FILE)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

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

/// Remove the agent's marker if it is of `kind`, leaving any other a hook
/// wrote since. Returns whether it removed one.
pub fn clear_waiting_if(
    project_root: &Path,
    scope: &str,
    agent: &str,
    kind: WaitingKind,
) -> Result<bool> {
    if read_waiting(project_root, scope, agent).is_some_and(|w| w.kind == kind) {
        clear_waiting(project_root, scope, agent)
    } else {
        Ok(false)
    }
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

/// Mark the agent for a restart at its next idle.
pub fn mark_restart_at_idle(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(RESTART_AT_IDLE_FILE);
    write_atomic(&file, b"")
}

pub fn restart_at_idle_marked(project_root: &Path, scope: &str, agent: &str) -> bool {
    agent_file(project_root, scope, agent, RESTART_AT_IDLE_FILE).exists()
}

pub fn clear_restart_at_idle(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    match std::fs::remove_file(agent_file(project_root, scope, agent, RESTART_AT_IDLE_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Append `line` to the agent's Stop hook log, stamped with the time and
/// this process's pid. Best-effort: a log that can't be written is skipped,
/// as is one for a project whose `.pm` is gone, which a hook signalled by
/// its project's delete would otherwise bring back.
pub fn log_stop_hook(project_root: &Path, scope: &str, agent: &str, line: &str) {
    if !paths::pm_dir(project_root).is_dir() {
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

/// A path the agent's current session reported at its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPath {
    /// The session's transcript.
    Transcript,
    /// The harness's config dir, as the agent's environment named it.
    ConfigDir,
}

impl SessionPath {
    fn file(self) -> &'static str {
        match self {
            Self::Transcript => "transcript",
            Self::ConfigDir => "config-dir",
        }
    }
}

/// Record `path` as the agent's current session's, or forget it (`None`).
pub fn write_session_path(
    project_root: &Path,
    scope: &str,
    agent: &str,
    which: SessionPath,
    path: Option<&Path>,
) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(which.file());
    match path {
        Some(path) => write_atomic(&file, path.to_string_lossy().as_bytes()),
        None => match std::fs::remove_file(file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        },
    }
}

/// The agent's current session's `which`, once the session has started
/// and reported one.
pub fn read_session_path(
    project_root: &Path,
    scope: &str,
    agent: &str,
    which: SessionPath,
) -> Option<PathBuf> {
    let text =
        std::fs::read_to_string(agent_file(project_root, scope, agent, which.file())).ok()?;
    Some(PathBuf::from(text))
}

/// The file whose existence says the turn end recorded by transcript entry
/// `entry` was claimed.
fn turn_end_claim(project_root: &Path, scope: &str, agent: &str, entry: &str) -> PathBuf {
    let name: String = entry
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    agent_file(
        project_root,
        scope,
        agent,
        &format!("{TURN_END_CLAIM}{name}"),
    )
}

/// Claim the turn end transcript entry `entry` records, one no hook
/// reported, for one caller only: true for the caller that claimed it.
/// Earlier claims are dropped. Keyed on the entry, not the transcript's
/// mtime, so bookkeeping written after it leaves the claim standing.
pub fn claim_turn_end(project_root: &Path, scope: &str, agent: &str, entry: &str) -> Result<bool> {
    let dir = agent_dir(project_root, scope, agent)?;
    let file = turn_end_claim(project_root, scope, agent, entry);
    match std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(&file)
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e.into()),
    }
    for dirent in std::fs::read_dir(dir)?.flatten() {
        let path = dirent.path();
        let stale = dirent
            .file_name()
            .to_string_lossy()
            .starts_with(TURN_END_CLAIM);
        if stale && path != file {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(true)
}

/// Give up a claim [`claim_turn_end`] made.
pub fn release_turn_end(project_root: &Path, scope: &str, agent: &str, entry: &str) -> Result<()> {
    match std::fs::remove_file(turn_end_claim(project_root, scope, agent, entry)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Whether the turn end transcript entry `entry` records has been claimed.
pub fn turn_end_claimed(project_root: &Path, scope: &str, agent: &str, entry: &str) -> bool {
    turn_end_claim(project_root, scope, agent, entry).exists()
}

/// Stamp the agent as active now.
pub fn touch_activity(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(ACTIVITY_FILE);
    std::fs::File::options()
        .create(true)
        .append(true)
        .open(file)?
        .set_modified(std::time::SystemTime::now())?;
    Ok(())
}

/// Stamp the agent as last active at `when`.
#[cfg(test)]
pub(crate) fn set_activity(project_root: &Path, scope: &str, agent: &str, when: DateTime<Utc>) {
    touch_activity(project_root, scope, agent).unwrap();
    std::fs::File::options()
        .append(true)
        .open(agent_file(project_root, scope, agent, ACTIVITY_FILE))
        .unwrap()
        .set_modified(when.into())
        .unwrap();
}

/// When the agent was last stamped active; `None` if it never was.
pub fn last_activity(project_root: &Path, scope: &str, agent: &str) -> Option<DateTime<Utc>> {
    let file = agent_file(project_root, scope, agent, ACTIVITY_FILE);
    Some(std::fs::metadata(file).ok()?.modified().ok()?.into())
}

/// The launch stamp's path, its directory created and the stamp removed:
/// what a spawn's typed line creates.
pub fn reset_launched(project_root: &Path, scope: &str, agent: &str) -> Result<PathBuf> {
    let file = agent_dir(project_root, scope, agent)?.join(LAUNCHED_FILE);
    match std::fs::remove_file(&file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(file),
    }
}

/// Where the agent's launch stamp is, whether or not it exists.
pub fn launched_file(project_root: &Path, scope: &str, agent: &str) -> PathBuf {
    agent_file(project_root, scope, agent, LAUNCHED_FILE)
}

/// When the agent's window ran its spawn's typed line; `None` until it has.
pub fn launched_at(project_root: &Path, scope: &str, agent: &str) -> Option<DateTime<Utc>> {
    let file = agent_file(project_root, scope, agent, LAUNCHED_FILE);
    Some(std::fs::metadata(file).ok()?.modified().ok()?.into())
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

/// The latest activity of any agent in the scope.
pub fn scope_last_activity(project_root: &Path, scope: &str) -> Option<DateTime<Utc>> {
    std::fs::read_dir(scope_dir(project_root, scope))
        .ok()?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name();
            last_activity(project_root, scope, name.to_str()?)
        })
        .max()
}

/// Remove one agent's runtime files.
pub fn remove_agent(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    remove(&scope_dir(project_root, scope).join(agent))
}

/// Remove the runtime files of every agent in a scope.
pub fn remove_scope(project_root: &Path, scope: &str) -> Result<()> {
    remove(&scope_dir(project_root, scope))
}

fn remove(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn runtime_files_stay_out_of_the_state_repo() {
        let dir = tempdir().unwrap();
        let pm_dir = paths::pm_dir(dir.path());
        std::fs::create_dir_all(&pm_dir).unwrap();
        crate::git::run_git(&pm_dir, &["init"]).unwrap();

        let agent = agent_dir(dir.path(), "login", "reviewer").unwrap();
        std::fs::write(agent.join("prompt.md"), "x").unwrap();

        let status =
            crate::git::run_git(&pm_dir, &["status", "--porcelain", "--untracked-files=all"])
                .unwrap();
        assert_eq!(status, "");
    }

    #[test]
    fn removal_is_scoped_to_the_agent_or_scope() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let reviewer = agent_dir(root, "login", "reviewer").unwrap();
        let implementer = agent_dir(root, "login", "implementer").unwrap();
        let other = agent_dir(root, "signup", "reviewer").unwrap();

        remove_agent(root, "login", "reviewer").unwrap();
        assert!(!reviewer.exists());
        assert!(implementer.exists());

        remove_scope(root, "login").unwrap();
        assert!(!implementer.exists());
        assert!(other.exists());

        // Removing what is already gone is not an error.
        remove_scope(root, "login").unwrap();
    }

    #[test]
    fn a_waiting_marker_round_trips_and_clearing_says_whether_there_was_one() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        assert_eq!(read_waiting(root, "login", "reviewer"), None);
        assert!(!clear_waiting(root, "login", "reviewer").unwrap());

        let waiting = Waiting::now(WaitingKind::HookEnded, Some("SIGTERM".into()));
        write_waiting(root, "login", "reviewer", &waiting).unwrap();
        assert_eq!(read_waiting(root, "login", "reviewer"), Some(waiting));

        assert!(clear_waiting(root, "login", "reviewer").unwrap());
        assert_eq!(read_waiting(root, "login", "reviewer"), None);
    }

    #[test]
    fn a_scope_is_as_recently_active_as_its_latest_agent() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        assert_eq!(scope_last_activity(root, "login"), None);
        agent_dir(root, "login", "qa").unwrap();
        set_activity(
            root,
            "login",
            "reviewer",
            Utc::now() - chrono::Duration::hours(1),
        );
        touch_activity(root, "login", "implementer").unwrap();

        let reviewer = last_activity(root, "login", "reviewer").unwrap();
        let implementer = last_activity(root, "login", "implementer").unwrap();
        assert!(Utc::now() - reviewer > chrono::Duration::minutes(59));
        assert!(Utc::now() - implementer < chrono::Duration::minutes(1));
        assert_eq!(scope_last_activity(root, "login"), Some(implementer));
    }

    #[test]
    fn a_turn_end_is_claimed_once_until_given_back() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let first = "6f1c2a8e-0001";
        let later = "6f1c2a8e-0002";

        assert!(claim_turn_end(root, "login", "qa", first).unwrap());
        assert!(!claim_turn_end(root, "login", "qa", first).unwrap());
        assert!(turn_end_claimed(root, "login", "qa", first));

        assert!(claim_turn_end(root, "login", "qa", later).unwrap());
        release_turn_end(root, "login", "qa", later).unwrap();
        assert!(!turn_end_claimed(root, "login", "qa", later));
        assert!(claim_turn_end(root, "login", "qa", later).unwrap());
    }
}
