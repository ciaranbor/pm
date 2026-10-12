//! The ready notice: the message telling `main` a feature is ready and
//! where its summary is.
//!
//! Marking a feature ready makes a notice *due*; it is sent once none of the
//! feature's agents is busy, so a feature marked ready mid-turn, or while
//! its reviewer still checks, is announced when its team stops. The marking
//! itself sends it when the team already is; otherwise each agent's waiter,
//! as it goes idle, sends it once it finds the rest idle too. Of two waiters
//! that do at once, one claims the due notice and the other finds it gone.
//! Leaving ready drops a due notice.
//!
//! A notice says only where the summary is, so marking ready again while
//! `main` has not answered the last one tells it nothing new: `main` reads
//! the summary as it is when it gets there. Within one ready period, a
//! feature is announced again only once `main` has sent its team a message
//! since the last notice and the summary differs from the one that notice
//! announced. After the feature has left ready for more work, a changed
//! summary is announced whether or not `main` answered. The merge or delete
//! notice ([`super::feat_delete`]) is separate and always sent.
//!
//! The record of what was last sent is the scope's own runtime state
//! ([`runtime::scope_file`]), so it goes with the feature.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::attention::{self, AgentState};
use crate::error::Result;
use crate::messages;
use crate::state::feature::Progress;
use crate::state::paths;
use crate::state::runtime;
use crate::state::scope::{self, TeamStatus};

const DUE: &str = ".ready-notice-due";
const SENT: &str = ".ready-notice-sent";

/// A notice waiting for the team to stop.
#[derive(Serialize, Deserialize)]
struct Due {
    /// The agent that marked the feature ready, which `main` can reply to;
    /// `None` when no agent did.
    agent: Option<String>,
}

/// The last notice sent.
#[derive(Serialize, Deserialize)]
struct Sent {
    at: DateTime<Utc>,
    /// The hash of the summary it announced.
    summary: String,
    /// The feature has left ready since.
    #[serde(default)]
    reopened: bool,
}

/// What marking a feature ready did about telling `main`.
#[derive(Debug, PartialEq, Eq)]
pub enum Notice {
    Sent,
    /// Due, to be sent once no agent of the team is busy.
    Due,
    /// `main` was told of this summary already.
    Unchanged,
    /// `main` has not answered the last notice yet.
    Unanswered,
}

impl Notice {
    /// What the marking says of it.
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Sent => "main is told",
            Self::Due => "main is told once no agent of its team is busy",
            Self::Unchanged => "main already has this summary",
            Self::Unanswered => {
                "main has not answered the last notice; it will read the summary as it is now"
            }
        }
    }
}

/// Note that `name` was marked ready by `agent`, and tell `main` now if
/// its team is idle.
pub fn mark_ready(
    project_root: &Path,
    name: &str,
    agent: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<Notice> {
    if let Some(sent) = read::<Sent>(&runtime::scope_file(project_root, name, SENT)?) {
        if sent.summary == summary_hash(project_root, name) {
            return Ok(Notice::Unchanged);
        }
        if !sent.reopened && !main_answered_since(project_root, name, sent.at) {
            return Ok(Notice::Unanswered);
        }
    }
    make_due(project_root, name, agent)?;
    Ok(if deliver(project_root, name, tmux_server)? {
        Notice::Sent
    } else {
        Notice::Due
    })
}

/// Make a notice for `name` due, from `agent`, for [`deliver`] to send.
pub fn make_due(project_root: &Path, name: &str, agent: Option<&str>) -> Result<()> {
    let due = Due {
        agent: agent.map(str::to_string),
    };
    crate::fs_utils::write_atomic(
        &runtime::scope_file(project_root, name, DUE)?,
        &serde_json::to_vec(&due)?,
    )
}

/// `name` is no longer ready: drop a notice still due, and note that the
/// last one sent belongs to an earlier ready period.
pub fn leave_ready(project_root: &Path, name: &str) -> Result<()> {
    match std::fs::remove_file(runtime::scope_file(project_root, name, DUE)?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let sent_path = runtime::scope_file(project_root, name, SENT)?;
    if let Some(sent) = read::<Sent>(&sent_path).filter(|s| !s.reopened) {
        let sent = Sent {
            reopened: true,
            ..sent
        };
        crate::fs_utils::write_atomic(&sent_path, &serde_json::to_vec(&sent)?)?;
    }
    Ok(())
}

/// Send `name`'s due notice if it is still ready and none of its agents is
/// busy. Returns whether this call sent it.
pub fn deliver(project_root: &Path, name: &str, tmux_server: Option<&str>) -> Result<bool> {
    let due_path = runtime::scope_file(project_root, name, DUE)?;
    if !due_path.exists()
        || TeamStatus::load(project_root, name)?.progress != Progress::Ready
        || attention::scope_agents(project_root, name, tmux_server)?
            .iter()
            .any(|a| a.state == AgentState::Busy)
    {
        return Ok(false);
    }
    let claimed = due_path.with_extension(std::process::id().to_string());
    match std::fs::rename(&due_path, &claimed) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        other => other?,
    }
    let due = read::<Due>(&claimed).unwrap_or(Due { agent: None });
    if let Err(e) = send(project_root, name, due.agent.as_deref()) {
        std::fs::rename(&claimed, &due_path)?;
        return Err(e);
    }
    std::fs::remove_file(&claimed)?;
    let record = Sent {
        at: Utc::now(),
        summary: summary_hash(project_root, name),
        reopened: false,
    };
    crate::fs_utils::write_atomic(
        &runtime::scope_file(project_root, name, SENT)?,
        &serde_json::to_vec(&record)?,
    )?;
    Ok(true)
}

fn send(project_root: &Path, name: &str, agent: Option<&str>) -> Result<()> {
    let sender = agent.map_or_else(messages::default_user_name, str::to_string);
    messages::send_with_scope(
        &paths::messages_dir(project_root),
        scope::MAIN,
        &scope::orchestrator(project_root),
        &sender,
        &body(project_root, name, agent.is_some()),
        Some(name),
    )?;
    Ok(())
}

/// The notice for `name`; `repliable` when an agent marked it ready.
fn body(project_root: &Path, name: &str, repliable: bool) -> String {
    let mut body = format!(
        "Feature '{name}' is ready: its work is done and waits on the user to merge or delete it. \
         Review its summary at {} for gaps; triage happens at merge or delete.",
        paths::summary_path(project_root, name).display()
    );
    if repliable {
        body.push_str(" The team is still running; reply here with any follow-up questions.");
    }
    body
}

/// Whether `main`'s agent has messaged any agent of `name` since `since`.
fn main_answered_since(project_root: &Path, name: &str, since: DateTime<Utc>) -> bool {
    let main = format!("{}@{}", scope::orchestrator(project_root), scope::MAIN);
    messages::last_sent(&paths::messages_dir(project_root), name, &main)
        .is_some_and(|at| at > since)
}

fn summary_hash(project_root: &Path, name: &str) -> String {
    let summary =
        std::fs::read_to_string(paths::summary_path(project_root, name)).unwrap_or_default();
    crate::hash::sha256_hex(summary.trim().as_bytes())
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feat_status::feat_status;
    use crate::testing::{NO_SERVER, TestServer};
    use tempfile::tempdir;

    fn notices(project: &Path) -> u32 {
        messages::unread_count(&paths::messages_dir(project), scope::MAIN, "main")
    }

    fn write_summary(project: &Path, text: &str) {
        std::fs::write(
            crate::commands::feat_summary::path(project, "login").unwrap(),
            text,
        )
        .unwrap();
    }

    fn ready(project: &Path, server: Option<&str>) -> Notice {
        feat_status(
            project,
            "login",
            Progress::Ready,
            None,
            Some("implementer"),
            server,
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn a_notice_waits_for_the_busy_team_and_is_sent_once() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = crate::tmux::session_name(&project_name, "login");
        let reviewer = server.spawn_fake_agent(&project, &session, "login", "reviewer");
        write_summary(&project, "notes");

        assert_eq!(ready(&project, server.name()), Notice::Due);
        assert!(!deliver(&project, "login", server.name()).unwrap());
        assert_eq!(notices(&project), 0);

        server.tmux_stdout(&["kill-window", "-t", &reviewer]);
        assert!(deliver(&project, "login", server.name()).unwrap());
        assert!(!deliver(&project, "login", server.name()).unwrap());
        assert_eq!(notices(&project), 1);
    }

    #[test]
    fn leaving_ready_drops_a_due_notice() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = crate::tmux::session_name(&project_name, "login");
        let reviewer = server.spawn_fake_agent(&project, &session, "login", "reviewer");
        write_summary(&project, "notes");

        assert_eq!(ready(&project, server.name()), Notice::Due);
        feat_status(&project, "login", Progress::Wip, None, None, server.name()).unwrap();
        server.tmux_stdout(&["kill-window", "-t", &reviewer]);

        assert!(!deliver(&project, "login", server.name()).unwrap());
        assert_eq!(notices(&project), 0);
    }

    #[test]
    fn main_is_told_again_only_of_a_new_summary_after_it_answered() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        write_summary(&project, "notes");
        assert_eq!(ready(&project, NO_SERVER), Notice::Sent);

        assert_eq!(ready(&project, NO_SERVER), Notice::Unchanged);
        write_summary(&project, "notes, revised");
        assert_eq!(ready(&project, NO_SERVER), Notice::Unanswered);
        assert_eq!(notices(&project), 1);

        messages::send_with_scope(
            &paths::messages_dir(&project),
            "login",
            "implementer",
            "main",
            "what about X?",
            Some(scope::MAIN),
        )
        .unwrap();
        assert_eq!(ready(&project, NO_SERVER), Notice::Sent);
        assert_eq!(notices(&project), 2);
    }

    #[test]
    fn a_new_summary_after_more_work_is_announced_unanswered() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        write_summary(&project, "notes");
        assert_eq!(ready(&project, NO_SERVER), Notice::Sent);

        feat_status(&project, "login", Progress::Wip, None, None, NO_SERVER).unwrap();
        assert_eq!(ready(&project, NO_SERVER), Notice::Unchanged);
        feat_status(&project, "login", Progress::Wip, None, None, NO_SERVER).unwrap();
        write_summary(&project, "notes, and the follow-up work");

        assert_eq!(ready(&project, NO_SERVER), Notice::Sent);
        assert_eq!(notices(&project), 2);
    }
}
