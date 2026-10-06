//! `pm tmux refresh`: the [`attention`] snapshot of every project, published
//! on the tmux server as `@pm_*` user options for status lines and tree
//! formats to read (docs/tmux.md, "Published options", has the contract).
//!
//! It runs on every watcher tick ([`tmux_watch`](super::tmux_watch)) and
//! every push ([`tmux_push`](super::tmux_push)), so it costs the snapshot's
//! own reads, one `tmux` call that takes what is published now along with the
//! attached clients, and, only when something changed, one more that writes
//! the difference. What it last published is also the previous state a
//! transition alert is judged against ([`transition`]); pm keeps no other
//! record of it, so refreshes of one server take turns on a lock keyed by
//! the server's socket, which names it however it was reached.
//!
//! A scope's agents, main's included, are found through the registry, never
//! by window, and only their windows carry options. A main session carries
//! `@pm_project` so its windows count as the project's when they are
//! cleared, and its main agent's badge, so the tree shows whether the
//! orchestrator is working or waiting on the user. Options are cleared only
//! on sessions of projects the snapshot read, or no longer registered: a
//! project whose state or registry entry couldn't be read keeps what it last
//! published.

use std::collections::HashSet;
use std::path::Path;

use crate::error::{PmError, Result};
use crate::tmux;
use crate::tmux::options::{self, Command, Holder, Options, Scope, format_text};

use chrono::{DateTime, Utc};

use super::attention::{self, AgentSnapshot, Snapshot};
use super::tmux_lock;

mod alert;
mod badge;
mod values;

use alert::{alert, announce};
use attention::transition::{self, Judged};
pub(crate) use values::window_values;
use values::{feature_entry, features_alerted, global_values, main_values, session_values};

const PROJECT: &str = "@pm_project";
const FEATURE: &str = "@pm_feature";
const PROGRESS: &str = "@pm_progress";
const REASON: &str = "@pm_reason";
const ATTENTION: &str = "@pm_attention";
const BADGE: &str = "@pm_badge";
const LABEL: &str = "@pm_label";
const ACTIVITY: &str = "@pm_activity";
const ACTIVITY_LABEL: &str = "@pm_activity_label";
const ALERTED: &str = "@pm_alerted";
const SESSION_OPTIONS: &[&str] = &[
    PROJECT,
    FEATURE,
    PROGRESS,
    REASON,
    ATTENTION,
    BADGE,
    LABEL,
    ACTIVITY,
    ACTIVITY_LABEL,
    ALERTED,
];

const AGENT: &str = "@pm_agent";
const AGENT_STATE: &str = "@pm_agent_state";
const UNREAD: &str = "@pm_unread";
const AGENT_BADGE: &str = "@pm_agent_badge";
const AGENT_LABEL: &str = "@pm_agent_label";
pub(super) const WINDOW_OPTIONS: &[&str] = &[AGENT, AGENT_STATE, UNREAD, AGENT_BADGE, AGENT_LABEL];

const ANNOUNCEMENT_HIDDEN: &str = "@pm_announcement_hidden";
const ANNOUNCEMENT_WINDOW: &str = "@pm_announcement_window";
/// What a refresh reads of each window: an agent's options, and the
/// announcement pair, which an announcement rather than the window's agent
/// sets and clears.
const WINDOW_READ: &[&str] = &[
    AGENT,
    AGENT_STATE,
    UNREAD,
    AGENT_BADGE,
    AGENT_LABEL,
    ANNOUNCEMENT_HIDDEN,
];

const SUMMARY: &str = "@pm_summary";
const COUNT: &str = "@pm_count";
const FEATURES_ALERTED: &str = "@pm_features_alerted";
const ANNOUNCEMENT: &str = "@pm_announcement";
const ANNOUNCEMENT_ID: &str = "@pm_announcement_id";
/// tmux's own option: how long a message shows, in milliseconds.
const DISPLAY_TIME: &str = "display-time";
const GLOBAL_READ: &[&str] = &[SUMMARY, COUNT, FEATURES_ALERTED, DISPLAY_TIME];

/// What separates [`FEATURES_ALERTED`] entries: a unit separator, which no
/// session name holds, where a space may be in a project's name.
const ENTRY_SEPARATOR: &str = "\x1f";

/// Publish the snapshot of every project registered in `projects_dir`. No
/// server running publishes nothing.
pub fn refresh(projects_dir: &Path, tmux_server: Option<&str>) -> Result<()> {
    let Some(socket) = tmux::socket_path(tmux_server)? else {
        return Ok(());
    };
    let _lock = tmux_lock::lock(&socket, "refresh")?;
    let Some(published) = options::read(tmux_server, SESSION_OPTIONS, WINDOW_READ, GLOBAL_READ)?
    else {
        return Ok(());
    };
    let snapshot = attention::all(projects_dir, tmux_server)?;
    write(tmux_server, &commands(&snapshot, &published, Utc::now()))
}

/// Run `commands`, the client tail last. A client that detached since the
/// read cuts that tail short, but the options are written by then.
fn write(tmux_server: Option<&str>, commands: &[Command]) -> Result<()> {
    match options::run(tmux_server, commands) {
        Err(PmError::Tmux(msg)) if msg.contains("can't find client") => Ok(()),
        result => result,
    }
}

/// What turns `published` into `snapshot`, as of `now`: the changed
/// options, the announcement of any alerts ([`announce`]), then a redraw
/// for each client.
fn commands(snapshot: &Snapshot, published: &Options, now: DateTime<Utc>) -> Vec<Command> {
    let mut writes = Vec::new();
    let mut alerts = Vec::new();
    let mut sessions: HashSet<&str> = HashSet::new();
    let mut windows: HashSet<&str> = HashSet::new();
    let mut needing = Vec::new();
    let recorded = features_alerted(published.global.get(FEATURES_ALERTED));
    let recorded_for = |session: &str| {
        recorded
            .iter()
            .find(|(s, _)| *s == session)
            .map(|(_, kinds)| *kinds)
    };
    // Nothing published yet: the server is new, and what a closed feature
    // needs was set before it.
    let first_publish = published.global.get(COUNT).is_empty();
    let mut record = Vec::new();
    for feature in &snapshot.features {
        // A session made since the read is published next time.
        let Some(held) = published
            .sessions
            .iter()
            .find(|s| s.target == feature.session)
        else {
            let mut kinds = recorded_for(&feature.session).map(str::to_string);
            if !feature.session_exists {
                let previous = match &kinds {
                    Some(kinds) => Some(Judged {
                        alerted: Judged::parse_alerted(kinds),
                        ..Judged::default()
                    }),
                    None if first_publish => None,
                    None => Some(Judged::default()),
                };
                let verdict = transition::judge_feature(previous.as_ref(), feature);
                if verdict.alert {
                    alerts.push(alert(&feature.session, &feature.attention, &[]));
                }
                kinds = verdict.judged.alerted_list();
            }
            record.extend(kinds.map(|kinds| feature_entry(&feature.session, &kinds)));
            continue;
        };
        sessions.insert(&feature.session);
        let verdict = transition::judge_feature(previous(held).as_ref(), feature);
        if verdict.alert {
            alerts.push(alert(&feature.session, &feature.attention, &feature.agents));
        }
        if let Some(kinds) = verdict.judged.alerted_list() {
            record.push(feature_entry(&feature.session, &kinds));
        }
        needing.extend(verdict.judged.attention);
        let values = session_values(feature, &verdict.judged, now);
        diff(&mut writes, Scope::Session(&feature.session), held, &values);
        agent_windows(&mut writes, &mut windows, published, &feature.agents);
    }
    for project in &snapshot.projects {
        let Some(main) = &project.main else {
            continue;
        };
        let Some(held) = published.sessions.iter().find(|s| s.target == main.session) else {
            continue;
        };
        sessions.insert(&main.session);
        let verdict = transition::judge_main(previous(held).as_ref(), main);
        if verdict.alert {
            alerts.push(alert(&main.session, &main.attention, &main.agents));
        }
        needing.extend(verdict.judged.attention);
        let values = main_values(&project.name, main, &verdict.judged, now);
        diff(&mut writes, Scope::Session(&main.session), held, &values);
        agent_windows(&mut writes, &mut windows, published, &main.agents);
    }

    let names = |skipped: bool| -> HashSet<String> {
        snapshot
            .projects
            .iter()
            .filter(|p| skipped || p.skipped.is_none())
            .map(|p| format_text(&p.name))
            .collect()
    };
    let (registered, read) = (names(true), names(false));
    // A project that couldn't be read keeps its features' record.
    for project in snapshot.projects.iter().filter(|p| p.skipped.is_some()) {
        let prefix = tmux::session_name(&project.name, "");
        record.extend(
            recorded
                .iter()
                .filter(|(session, _)| session.starts_with(&prefix))
                .map(|(session, kinds)| feature_entry(session, kinds)),
        );
    }
    let ours = |session: &str| {
        published.sessions.iter().any(|s| {
            let project = s.get(PROJECT);
            s.target == session
                && !project.is_empty()
                && (read.contains(project) || !registered.contains(project))
        })
    };
    for held in &published.sessions {
        if ours(&held.target) && !sessions.contains(held.target.as_str()) {
            clear(
                &mut writes,
                Scope::Session(&held.target),
                held,
                SESSION_OPTIONS,
            );
        }
    }
    for held in &published.windows {
        if !held.get(AGENT).is_empty()
            && ours(&held.session)
            && !windows.contains(held.target.as_str())
        {
            clear(
                &mut writes,
                Scope::Window(&held.target),
                held,
                WINDOW_OPTIONS,
            );
        }
    }

    diff(
        &mut writes,
        Scope::Global,
        &published.global,
        &global_values(needing, &record),
    );

    writes.extend(announce(&alerts, published, now));
    if !writes.is_empty() {
        writes.extend(
            published
                .clients
                .iter()
                .map(|c| options::refresh_status(&c.name)),
        );
    }
    writes
}

/// Publish each of `agents`' windows, adding them to `windows`.
fn agent_windows<'a>(
    writes: &mut Vec<Command>,
    windows: &mut HashSet<&'a str>,
    published: &Options,
    agents: &'a [AgentSnapshot],
) {
    for agent in agents {
        let Some(window) = agent.window.as_deref() else {
            continue;
        };
        let Some(held) = published.windows.iter().find(|w| w.target == window) else {
            continue;
        };
        windows.insert(window);
        diff(writes, Scope::Window(window), held, &window_values(agent));
    }
}

/// What a session's options record of its last judgement; `None` for a
/// session pm has not published to yet, which was just opened.
fn previous(held: &Holder) -> Option<Judged> {
    if held.get(PROJECT).is_empty() {
        return None;
    }
    Some(Judged {
        attention: Judged::parse_attention(held.get(ATTENTION)),
        alerted: Judged::parse_alerted(held.get(ALERTED)),
    })
}

/// Set each of `values` that differs from what `held` has; a `None` value
/// is unset.
fn diff(writes: &mut Vec<Command>, scope: Scope, held: &Holder, values: &[(&str, Option<String>)]) {
    for (name, value) in values {
        let now = held.get(name);
        let want = value.as_deref().unwrap_or_default();
        if now != want {
            writes.push(options::set(scope, name, value.as_deref()));
        }
    }
}

fn clear(writes: &mut Vec<Command>, scope: Scope, held: &Holder, names: &[&str]) {
    for name in names.iter().filter(|n| !held.get(n).is_empty()) {
        writes.push(options::set(scope, name, None));
    }
}

#[cfg(test)]
mod tests;
