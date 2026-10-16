//! `refresh` end to end, one module per concern; the snapshot builders and
//! readers of what a refresh published live here.

mod alerts;
mod badges;
mod failures;
mod main_session;
mod publishing;

use super::alert::test_support::*;
use super::*;
use crate::commands::attention::{
    AgentState, Attention, AttentionKind, FeatureSnapshot, ScopeSnapshot,
};
use crate::commands::tmux_init::TREE_FORMAT;
use crate::commands::{feat_delete::feat_delete, feat_status::feat_status};
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::feature::{FeatureState, FeatureStatus, Progress};
use crate::state::paths;
use crate::state::runtime::WaitingKind;
use crate::state::scope::TeamStatus;
use crate::testing::{OwnServer, TestServer, server_socket_exists};
use crate::tmux;
use crate::tmux::options::Client;
use std::sync::{Mutex, MutexGuard};
use tempfile::tempdir;

/// A refresh writes server-wide options, announcements included, so
/// these tests take turns on the shared server.
fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn published(server: &TestServer) -> Options {
    options::read(server.name(), SESSION_OPTIONS, WINDOW_READ, GLOBAL_READ)
        .unwrap()
        .unwrap()
}

fn values<'a>(holders: &'a [Holder], target: &str, names: &[&str]) -> Vec<&'a str> {
    let holder = holders.iter().find(|h| h.target == target).unwrap();
    names.iter().map(|n| holder.get(n)).collect()
}

fn main_scope(agents: Vec<AgentSnapshot>) -> Snapshot {
    Snapshot {
        version: attention::VERSION,
        projects: vec![attention::ProjectSnapshot {
            name: "app".into(),
            root: "/src/app".into(),
            skipped: None,
            home: false,
            main: Some(ScopeSnapshot::main_of(agents, TeamStatus::default())),
        }],
        features: Vec::new(),
    }
}

fn main_agent(state: AgentState, kind: WaitingKind, detail: &str) -> AgentSnapshot {
    AgentSnapshot {
        name: "main".into(),
        state,
        unread: 0,
        window: Some("app/main:1".into()),
        pane: None,
        waiting: Some(attention::WaitingSnapshot {
            kind,
            detail: detail.into(),
            since: None,
            dialog: None,
        }),
        latest: None,
        denied: None,
    }
}

fn sets<'a>(commands: &'a [Command], name: &str) -> Vec<&'a str> {
    commands
        .iter()
        .filter(|c| c[0] == "set-option" && c.iter().any(|a| a == name))
        .filter_map(|c| c.last().map(String::as_str))
        .filter(|v| *v != name)
        .collect()
}

fn announced(commands: &[Command]) -> Vec<&str> {
    sets(commands, ANNOUNCEMENT)
}

/// The announcements made on a server, each recorded once: its
/// display time is long enough that none is cleared meanwhile.
struct Announcements<'a> {
    server: &'a OwnServer,
    last_id: String,
    texts: Vec<String>,
}

impl<'a> Announcements<'a> {
    fn held(server: &'a OwnServer) -> Self {
        server.tmux_stdout(&["set", "-g", DISPLAY_TIME, "600000"]);
        Self {
            server,
            last_id: String::new(),
            texts: Vec::new(),
        }
    }

    /// Record the announcement up now, if it is a new one.
    fn take(&mut self) {
        let held = options::read_global(self.server.name(), &[ANNOUNCEMENT, ANNOUNCEMENT_ID])
            .unwrap()
            .unwrap();
        if held.get(ANNOUNCEMENT_ID) != self.last_id {
            self.last_id = held.get(ANNOUNCEMENT_ID).to_string();
            self.texts.push(held.get(ANNOUNCEMENT).to_string());
        }
    }
}

fn feature_scope(progress: Progress, busy: bool, agents: Vec<AgentSnapshot>) -> Snapshot {
    let mut feature = FeatureSnapshot {
        project: "app".into(),
        name: "login".into(),
        attention: Attention {
            kind: AttentionKind::None,
            detail: None,
            agent: None,
        },
        progress,
        blocked_reason: None,
        blocked_by: None,
        summary: Some("Adds login".into()),
        lifecycle: FeatureStatus::Review,
        pr: None,
        session: "app/login".into(),
        session_exists: true,
        agents,
        working: busy,
        background_since: None,
        last_activity: None,
    };
    feature.attention = attention::attention(&feature);
    Snapshot {
        version: attention::VERSION,
        projects: Vec::new(),
        features: vec![feature],
    }
}

fn ready_feature(busy: bool) -> Snapshot {
    let state = if busy {
        AgentState::Busy
    } else {
        AgentState::Idle
    };
    feature_scope(Progress::Ready, busy, vec![agent_in(state)])
}

/// Publish `snapshot` on `server` as a refresh would, returning what is
/// published after.
fn publish(server: &OwnServer, snapshot: &Snapshot) -> Options {
    let read = || {
        options::read(server.name(), SESSION_OPTIONS, WINDOW_READ, GLOBAL_READ)
            .unwrap()
            .unwrap()
    };
    options::run(server.name(), &commands(snapshot, &read(), Utc::now())).unwrap();
    read()
}

/// A session pm has published to before.
fn published_session(server: &OwnServer, dir: &Path, session: &str) {
    tmux::create_session(server.name(), session, dir).unwrap();
    options::run(
        server.name(),
        &[options::set(Scope::Session(session), PROJECT, Some("app"))],
    )
    .unwrap();
}

fn agent_in(state: AgentState) -> AgentSnapshot {
    AgentSnapshot {
        name: "implementer".into(),
        state,
        unread: 0,
        window: Some("app/login:1".into()),
        pane: None,
        waiting: None,
        latest: None,
        denied: None,
    }
}
