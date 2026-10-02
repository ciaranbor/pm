//! Per-agent runtime files: what a spawn hands its harness (the composed
//! prompt, opencode's config) and what the harness leaves for pm (opencode's
//! stopped-loop marker, the waiting marker, the activity stamp).
//!
//! The **waiting marker** says what an agent whose Stop hook isn't running
//! is at: a dialog waiting on the user, its prompt with nothing to wake it,
//! or background work that will. pm's hooks write and clear it, and every
//! spawn replaces it. It only refines an agent the window reads as busy: a
//! running Stop hook or an exited harness says more, so a stale marker
//! never hides either.
//!
//! The **activity stamp** is a file whose mtime is the agent's last sign of
//! life: every pm hook invocation touches it.
//!
//! The **transcript record** holds the path of the current session's
//! transcript, written at SessionStart, for a harness that records an
//! interrupt only there ([`Harness::interrupted`](crate::harness::Harness::interrupted)).
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

const WAITING_FILE: &str = "waiting.json";
const ACTIVITY_FILE: &str = "activity";
const TRANSCRIPT_FILE: &str = "transcript";

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

/// What an agent whose Stop hook isn't running is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WaitingKind {
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
            Self::Interrupted | Self::HookEnded | Self::Error | Self::Prompt | Self::Tripped => {
                WaitingClass::Unarmed
            }
            Self::Background => WaitingClass::Background,
        }
    }

    /// What the kind says when the marker carries no detail.
    pub fn label(self) -> &'static str {
        match self {
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
}

impl Waiting {
    pub fn now(kind: WaitingKind, detail: Option<String>) -> Self {
        Self {
            kind,
            detail: detail.filter(|d| !d.trim().is_empty()),
            since: Utc::now(),
            subagent: None,
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

/// Record where the agent's current session keeps its transcript, as its
/// SessionStart hook reported it.
pub fn write_transcript(project_root: &Path, scope: &str, agent: &str, path: &Path) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(TRANSCRIPT_FILE);
    write_atomic(&file, path.to_string_lossy().as_bytes())
}

/// The agent's current session's transcript, once its session has started.
pub fn read_transcript(project_root: &Path, scope: &str, agent: &str) -> Option<PathBuf> {
    let text =
        std::fs::read_to_string(agent_file(project_root, scope, agent, TRANSCRIPT_FILE)).ok()?;
    Some(PathBuf::from(text))
}

/// Forget the agent's transcript, for a spawn whose session has not
/// started yet.
pub fn clear_transcript(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    match std::fs::remove_file(agent_file(project_root, scope, agent, TRANSCRIPT_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
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
}
