//! `pm tmux refresh`: the [`attention`] snapshot of every project, published
//! on the tmux server as `@pm_*` user options for status lines and tree
//! formats to read (README, "tmux integration", has the contract).
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

use super::attention::{
    self, Activity, AgentSnapshot, AgentState, Attention, AttentionKind, FeatureSnapshot,
    ScopeSnapshot, Snapshot,
};
use super::feat_status_view::{STALLED, span};
use super::tmux_lock;

mod badge;

use attention::transition::{self, Judged};

const PROJECT: &str = "@pm_project";
const FEATURE: &str = "@pm_feature";
const PROGRESS: &str = "@pm_progress";
const REASON: &str = "@pm_reason";
const ATTENTION: &str = "@pm_attention";
const BADGE: &str = "@pm_badge";
const LABEL: &str = "@pm_label";
const ACTIVITY: &str = "@pm_activity";
const ACTIVITY_LABEL: &str = "@pm_activity_label";
const ALERT_PENDING: &str = "@pm_alert_pending";
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
    ALERT_PENDING,
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
        let mut values = session_values(feature, &verdict.judged, now);
        values.push((ALERT_PENDING, verdict.judged.owed.then(|| "1".into())));
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
        owed: !held.get(ALERT_PENDING).is_empty(),
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

/// A feature session's options. Its badge shows its attention even while
/// its judgement leaves it out of the attention tree and the count.
fn session_values(
    feature: &FeatureSnapshot,
    judged: &Judged,
    now: DateTime<Utc>,
) -> Vec<(&'static str, Option<String>)> {
    let kind = feature.attention.kind;
    let needs = (kind != AttentionKind::None).then_some(kind);
    let [activity, activity_label] = attention::activity(
        feature.working,
        feature.background_since,
        feature.last_activity,
        now,
    )
    .map(|a| drawn(a, now).map(Some))
    .unwrap_or_default();
    vec![
        (PROJECT, Some(format_text(&feature.project))),
        (FEATURE, Some(format_text(&feature.name))),
        (PROGRESS, Some(feature.progress.to_string())),
        (REASON, reason(&feature.attention)),
        (ATTENTION, judged.attention.map(|k| k.to_string())),
        (BADGE, needs.and_then(badge::attention)),
        (LABEL, needs.and_then(badge::attention_label)),
        (ACTIVITY, activity),
        (ACTIVITY_LABEL, activity_label),
        (ALERTED, judged.alerted_list()),
    ]
}

/// The attention detail; a stalled scope has none of its own, so it gets
/// what the status view says.
fn reason(attention: &Attention) -> Option<String> {
    let detail = match attention.kind {
        AttentionKind::Stalled => Some(STALLED),
        _ => attention.detail.as_deref(),
    };
    detail.map(format_text).filter(|r| !r.is_empty())
}

/// A main session has no feature or progress. Its badge is its main
/// agent's, whatever its attention.
fn main_values(
    project: &str,
    main: &ScopeSnapshot,
    judged: &Judged,
    now: DateTime<Utc>,
) -> Vec<(&'static str, Option<String>)> {
    let lead = main
        .agents
        .iter()
        .find(|a| a.name == "main")
        .or(main.agents.first());
    let [activity, activity_label] = main_activity(main, lead, now)
        .map(|a| drawn(a, now).map(Some))
        .unwrap_or_default();
    vec![
        (PROJECT, Some(format_text(project))),
        (FEATURE, None),
        (PROGRESS, None),
        (REASON, reason(&main.attention)),
        (ATTENTION, judged.attention.map(|k| k.to_string())),
        (BADGE, lead.map(|a| badge::agent(a.state, a.unread))),
        (LABEL, lead.map(|a| badge::agent_label(a.state, a.unread))),
        (ACTIVITY, activity),
        (ACTIVITY_LABEL, activity_label),
        (ALERTED, judged.alerted_list()),
    ]
}

/// A main scope's activity, without the busy glyph its `lead`'s badge
/// already shows. Background work still shows, for its age.
fn main_activity(
    main: &ScopeSnapshot,
    lead: Option<&AgentSnapshot>,
    now: DateTime<Utc>,
) -> Option<Activity> {
    let lead_busy = lead.is_some_and(|a| a.state == AgentState::Busy);
    attention::activity(main.working, main.background_since, main.last_activity, now)
        .filter(|a| !(*a == Activity::Working && lead_busy))
}

/// `activity` as `@pm_activity` and `@pm_activity_label`: the busy glyph,
/// the background glyph and wait (`1d`), or the quiet spell (`2h`); then
/// the same with words.
fn drawn(activity: Activity, now: DateTime<Utc>) -> [String; 2] {
    match activity {
        Activity::Working => [badge::working(), badge::working_label()],
        Activity::Background(since) => {
            let span = span(since, now);
            [
                badge::background(&span),
                badge::background(&format!("background {span}")),
            ]
        }
        Activity::Quiet(since) => {
            let span = span(since, now);
            [
                badge::styled(QUIET_STYLE, &span),
                badge::styled(QUIET_STYLE, &format!("quiet {span}")),
            ]
        }
    }
}

const QUIET_STYLE: &str = "fg=colour245";

pub(super) fn window_values(agent: &AgentSnapshot) -> Vec<(&'static str, Option<String>)> {
    vec![
        (AGENT, Some(format_text(&agent.name))),
        (AGENT_STATE, Some(agent.state.to_string())),
        (UNREAD, Some(agent.unread.to_string())),
        (AGENT_BADGE, Some(badge::agent(agent.state, agent.unread))),
        (
            AGENT_LABEL,
            Some(badge::agent_label(agent.state, agent.unread)),
        ),
    ]
}

/// The totals of `kinds`, the attention of each session that publishes
/// one, so the count matches what the attention tree lists.
/// The `(session, alerted kinds)` entries of a [`FEATURES_ALERTED`] value,
/// which keeps every feature's alerted kinds so a closed feature, which has
/// no session to hold them, is judged against them too.
fn features_alerted(value: &str) -> Vec<(&str, &str)> {
    value
        .split(ENTRY_SEPARATOR)
        .filter_map(|entry| entry.rsplit_once('='))
        .collect()
}

fn feature_entry(session: &str, kinds: &str) -> String {
    format!("{session}={kinds}")
}

fn global_values(
    mut kinds: Vec<AttentionKind>,
    record: &[String],
) -> Vec<(&'static str, Option<String>)> {
    kinds.sort();
    let summary: Vec<String> = kinds
        .chunk_by(|a, b| a == b)
        .filter_map(|run| badge::attention_count(run[0], run.len()))
        .collect();
    vec![
        (SUMMARY, Some(summary.join(" · ")).filter(|s| !s.is_empty())),
        (COUNT, Some(kinds.len().to_string())),
        (
            FEATURES_ALERTED,
            Some(record.join(ENTRY_SEPARATOR)).filter(|v| !v.is_empty()),
        ),
    ]
}

struct Alert<'a> {
    text: String,
    /// The pane of the agent asking, whose window's viewers it doesn't need
    /// to reach.
    pane: Option<&'a str>,
}

fn alert<'a>(session: &str, attention: &Attention, agents: &'a [AgentSnapshot]) -> Alert<'a> {
    let what = format!("{session} {}", attention.kind);
    let pane = agents
        .iter()
        .filter(|_| attention.kind == AttentionKind::Asking)
        .find(|a| attention.agent.as_ref() == Some(&a.name))
        .and_then(|a| a.pane.as_deref());
    Alert {
        text: match &attention.detail {
            Some(detail) => format!("{what}: {detail}"),
            None => what,
        },
        pane,
    }
}

/// What a status line shows `alerts` with, for as long as tmux shows a
/// message (`display-time`), as of `now`: the text in [`ANNOUNCEMENT`];
/// on each asking agent's window, [`ANNOUNCEMENT_HIDDEN`] and the text
/// without that ask in [`ANNOUNCEMENT_WINDOW`]; and a server-side timer
/// clearing them unless a newer announcement, with another
/// [`ANNOUNCEMENT_ID`], has replaced them. The pair an earlier
/// announcement set comes off first. No alerts, no change.
fn announce(alerts: &[Alert], published: &Options, now: DateTime<Utc>) -> Vec<Command> {
    if alerts.is_empty() {
        return Vec::new();
    }
    let joined = |pane: Option<&str>| -> String {
        let texts: Vec<&str> = alerts
            .iter()
            .filter(|a| pane.is_none() || a.pane != pane)
            .map(|a| a.text.as_str())
            .collect();
        if texts.is_empty() {
            String::new()
        } else {
            format_text(&format!("pm: {}", texts.join(" · ")))
        }
    };
    let id = now.timestamp_micros().to_string();
    let mut writes = Vec::new();
    for held in published
        .windows
        .iter()
        .filter(|w| !w.get(ANNOUNCEMENT_HIDDEN).is_empty())
    {
        for name in [ANNOUNCEMENT_HIDDEN, ANNOUNCEMENT_WINDOW] {
            writes.push(options::set(Scope::Window(&held.target), name, None));
        }
    }
    writes.push(options::set(
        Scope::Global,
        ANNOUNCEMENT,
        Some(&joined(None)),
    ));
    writes.push(options::set(Scope::Global, ANNOUNCEMENT_ID, Some(&id)));
    let mut clear = format!("set -gqu {ANNOUNCEMENT} ; set -gqu {ANNOUNCEMENT_ID}");
    let panes: std::collections::BTreeSet<&str> = alerts.iter().filter_map(|a| a.pane).collect();
    for pane in panes {
        let window = Scope::PaneWindow(pane);
        writes.push(options::set(window, ANNOUNCEMENT_HIDDEN, Some("1")));
        writes.push(options::set(
            window,
            ANNOUNCEMENT_WINDOW,
            Some(&joined(Some(pane))),
        ));
        for name in [ANNOUNCEMENT_HIDDEN, ANNOUNCEMENT_WINDOW] {
            clear.push_str(&format!(" ; set -wqu -t {pane} {name}"));
        }
    }
    // The format is expanded when the timer is set, so the guard is
    // escaped to be read when it fires.
    let guard = format!("if -F \"##{{==:##{{{ANNOUNCEMENT_ID}}},{id}}}\" \"{clear}\"");
    writes.push(options::run_shell_later(
        &display_seconds(published.global.get(DISPLAY_TIME)),
        &guard,
    ));
    writes
}

/// `display_time`, tmux's milliseconds, as seconds. `0`, which keeps a
/// message up until a key is pressed, has no equivalent on the status line
/// and takes tmux's default instead.
fn display_seconds(display_time: &str) -> String {
    let millis = display_time
        .parse::<u32>()
        .ok()
        .filter(|ms| *ms > 0)
        .unwrap_or(750);
    format!("{:.3}", f64::from(millis) / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tmux_init::TREE_FORMAT;
    use crate::commands::{feat_delete::feat_delete, feat_status::feat_status};
    use crate::messages;
    use crate::state::agent::AgentRegistry;
    use crate::state::feature::{FeatureState, FeatureStatus, Progress};
    use crate::state::paths;
    use crate::state::runtime::WaitingKind;
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

    #[test]
    fn options_are_published_and_cleared_once_their_value_goes_away() {
        let _serial = serial();
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project);
        let session = tmux::session_name(&project_name, "login");
        let implementer = server.spawn_idle_fake_agent(&project, &session, "login", "implementer");
        let reviewer = server.spawn_fake_agent(&project, &session, "login", "reviewer");
        messages::send(
            &paths::messages_dir(&project),
            "login",
            "reviewer",
            "user",
            "hi",
        )
        .unwrap();
        let shell =
            tmux::new_window(server.name(), &session, &project, Some("shell"), true).unwrap();
        let mine = server.scope("mine");
        tmux::create_session(server.name(), &mine, dir.path()).unwrap();
        options::run(
            server.name(),
            &[
                options::set(Scope::Session(&mine), BADGE, Some("mine")),
                options::set(Scope::Window(&shell), AGENT_BADGE, Some("mine")),
            ],
        )
        .unwrap();
        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which #[fg=red]DB?"),
            Some("implementer"),
        )
        .unwrap();

        refresh(&projects_dir, server.name()).unwrap();

        let now = published(&server);
        assert_eq!(
            values(&now.sessions, &session, SESSION_OPTIONS),
            [
                project_name.as_str(),
                "login",
                "blocked",
                "which ##[fg=red]DB?",
                "blocked",
                "#[fg=red,bold]\u{f256}#[default]",
                "#[fg=red,bold]\u{f256} blocked#[default]",
                "",
                "",
                "",
                "blocked",
            ]
        );
        let idle_label = "#[fg=colour245]\u{f252} idle#[default]";
        assert_eq!(
            values(&now.windows, &implementer, WINDOW_OPTIONS),
            [
                "implementer",
                "idle",
                "0",
                "#[fg=colour245]\u{f252}#[default]",
                idle_label,
            ]
        );
        let busy_label = "#[fg=green]\u{f013} busy#[default] #[fg=yellow]\u{f0e0} 1#[default]";
        assert_eq!(
            values(&now.windows, &reviewer, WINDOW_OPTIONS),
            [
                "reviewer",
                "busy",
                "1",
                "#[fg=green]\u{f013} #[fg=yellow]\u{f0e0}#[default]",
                busy_label,
            ]
        );
        assert_eq!(
            values(&now.windows, &shell, WINDOW_OPTIONS),
            ["", "", "", "mine", ""],
            "a window that is no agent's"
        );
        let display = |target: &str, format: &str| {
            server.tmux_stdout(&["display", "-p", "-t", target, format])
        };
        // `display` runs in a pane's context: each line of the tree is its
        // branch of the format taken by hand.
        let tree_line = |window| {
            TREE_FORMAT
                .replacen("#{?pane_format,", "#{?@pm_none,", 1)
                .replacen(
                    "#{?window_format,",
                    if window {
                        "#{?window_name,"
                    } else {
                        "#{?@pm_none,"
                    },
                    1,
                )
        };
        let window_line = tree_line(true);
        for (window, label) in [(&implementer, idle_label), (&reviewer, busy_label)] {
            assert_eq!(
                display(window, &window_line),
                format!(
                    "{} {label}",
                    display(window, "#{window_name}#{window_flags}")
                ),
                "a window's line in pm's tree"
            );
        }
        assert_eq!(
            display(&session, &tree_line(false)),
            format!(
                "{} windows #[fg=red,bold]\u{f256} blocked#[default]  which ##[fg=red]DB?",
                display(&session, "#{session_windows}"),
            ),
            "a session's line in pm's tree"
        );
        assert_eq!(
            [now.global.get(COUNT), now.global.get(SUMMARY)],
            ["1", "#[fg=red,bold]\u{f256} 1 blocked#[default]"]
        );

        feat_status(&project, "login", Progress::Wip, None, None).unwrap();
        let agents_dir = paths::agents_dir(&project);
        let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        registry.get_mut("implementer").unwrap().active = false;
        registry.agents.remove("reviewer");
        registry.save(&agents_dir, "login").unwrap();

        refresh(&projects_dir, server.name()).unwrap();

        let now = published(&server);
        assert_eq!(
            values(&now.sessions, &session, SESSION_OPTIONS),
            [
                project_name.as_str(),
                "login",
                "wip",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                ""
            ]
        );
        assert_eq!(
            values(&now.windows, &implementer, WINDOW_OPTIONS),
            [
                "implementer",
                "stopped",
                "0",
                "#[fg=colour245]\u{f04d}#[default]",
                "#[fg=colour245]\u{f04d} stopped#[default]",
            ]
        );
        assert_eq!(
            values(&now.windows, &reviewer, WINDOW_OPTIONS),
            ["", "", "", "", ""],
            "a window whose agent is gone from the registry"
        );
        assert_eq!([now.global.get(COUNT), now.global.get(SUMMARY)], ["0", ""]);
        assert_eq!(values(&now.windows, &shell, &[AGENT_BADGE]), ["mine"]);
        assert_eq!(
            values(&now.sessions, &mine, &[PROJECT, BADGE]),
            ["", "mine"],
            "a session that is no feature's"
        );
    }

    #[test]
    fn main_agents_get_badges_and_main_carries_its_project_and_its_agents_badge() {
        let _serial = serial();
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project);
        let main = tmux::session_name(&project_name, "main");
        let orchestrator = server.spawn_idle_fake_agent(&project, &main, "main", "main");
        messages::send(&paths::messages_dir(&project), "main", "main", "user", "hi").unwrap();

        refresh(&projects_dir, server.name()).unwrap();

        let now = published(&server);
        assert_eq!(
            values(&now.windows, &orchestrator, WINDOW_OPTIONS),
            [
                "main",
                "idle",
                "1",
                "#[fg=colour245]\u{f252} #[fg=yellow]\u{f0e0}#[default]",
                "#[fg=colour245]\u{f252} idle#[default] #[fg=yellow]\u{f0e0} 1#[default]",
            ]
        );
        assert_eq!(
            values(&now.sessions, &main, SESSION_OPTIONS),
            [
                project_name.as_str(),
                "",
                "",
                "",
                "",
                "#[fg=colour245]\u{f252} #[fg=yellow]\u{f0e0}#[default]",
                "#[fg=colour245]\u{f252} idle#[default] #[fg=yellow]\u{f0e0} 1#[default]",
                "",
                "",
                "",
                ""
            ],
            "main's own badge, whatever its attention"
        );

        let agents_dir = paths::agents_dir(&project);
        let mut registry = AgentRegistry::load(&agents_dir, "main").unwrap();
        registry.agents.remove("main");
        registry.save(&agents_dir, "main").unwrap();
        refresh(&projects_dir, server.name()).unwrap();

        let now = published(&server);
        assert_eq!(
            values(&now.windows, &orchestrator, WINDOW_OPTIONS),
            ["", "", "", "", ""]
        );
    }

    #[test]
    fn a_feature_that_goes_takes_its_session_options_with_it() {
        let _serial = serial();
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project);
        crate::commands::feat_new::feat_new(
            &crate::commands::feat_new::FeatNewParams::with_defaults(
                &project,
                &projects_dir,
                "search",
                server.name(),
            ),
        )
        .unwrap();
        let features_dir = paths::features_dir(&project);
        let mut search = FeatureState::load(&features_dir, "search").unwrap();
        search.status = FeatureStatus::Merged;
        search.save(&features_dir, "search").unwrap();
        let login = tmux::session_name(&project_name, "login");
        let search = tmux::session_name(&project_name, "search");

        refresh(&projects_dir, server.name()).unwrap();
        let now = published(&server);
        assert_eq!(values(&now.sessions, &search, &[ATTENTION]), ["cleanup"]);
        assert_eq!(
            [now.global.get(COUNT), now.global.get(SUMMARY)],
            ["1", "#[fg=colour245]\u{f00e2} 1 cleanup#[default]"]
        );

        feat_delete(&project, &projects_dir, "search", true, server.name()).unwrap();
        // State gone, session left open.
        std::fs::remove_file(features_dir.join("login.toml")).unwrap();
        refresh(&projects_dir, server.name()).unwrap();

        let now = published(&server);
        assert!(!now.sessions.iter().any(|s| s.target == search));
        assert_eq!(
            values(&now.sessions, &login, SESSION_OPTIONS),
            ["", "", "", "", "", "", "", "", "", "", ""]
        );
        assert_eq!([now.global.get(COUNT), now.global.get(SUMMARY)], ["0", ""]);
    }

    #[test]
    fn a_project_removed_from_the_registry_takes_its_options_with_it() {
        let _serial = serial();
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project);
        let session = tmux::session_name(&project_name, "login");
        let window = server.spawn_idle_fake_agent(&project, &session, "login", "implementer");
        refresh(&projects_dir, server.name()).unwrap();
        assert_eq!(
            values(&published(&server).windows, &window, &[AGENT]),
            ["implementer"]
        );

        std::fs::remove_file(projects_dir.join(format!("{project_name}.toml"))).unwrap();
        refresh(&projects_dir, server.name()).unwrap();

        let now = published(&server);
        let main = tmux::session_name(&project_name, "main");
        for session in [&session, &main] {
            assert!(
                values(&now.sessions, session, SESSION_OPTIONS)
                    .iter()
                    .all(|v| v.is_empty()),
                "{session}"
            );
        }
        assert_eq!(
            values(&now.windows, &window, WINDOW_OPTIONS),
            ["", "", "", "", ""]
        );
    }

    #[test]
    fn clients_are_alerted_once_when_a_feature_becomes_blocked_or_ready() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("alerts");
        let project = dir.path().join("app");
        let projects_dir = dir.path().join("registry");
        crate::commands::init::init(&project, &projects_dir, None, server.name()).unwrap();
        crate::commands::feat_new::feat_new(
            &crate::commands::feat_new::FeatNewParams::with_defaults(
                &project,
                &projects_dir,
                "login",
                server.name(),
            ),
        )
        .unwrap();
        let session = tmux::session_name("app", "login");
        let mut announced = Announcements::held(&server);

        refresh(&projects_dir, server.name()).unwrap();
        announced.take();
        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            None,
        )
        .unwrap();
        refresh(&projects_dir, server.name()).unwrap();
        announced.take();
        refresh(&projects_dir, server.name()).unwrap();
        announced.take();
        let summary = paths::summary_path(&project, "login");
        std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
        std::fs::write(&summary, "Adds login\n").unwrap();
        feat_status(&project, "login", Progress::Ready, None, None).unwrap();
        refresh(&projects_dir, server.name()).unwrap();
        announced.take();
        refresh(&projects_dir, server.name()).unwrap();
        announced.take();

        assert_eq!(
            announced.texts,
            [
                format!("pm: {session} blocked: which DB?"),
                format!("pm: {session} ready: Adds login"),
            ]
        );
    }

    #[test]
    fn changes_are_written_in_one_tmux_call_and_a_repeat_writes_nothing() {
        let _serial = serial();
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project);
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project, &session, "login", "implementer");
        server.tmux_stdout(&["set-option", "-s", "message-limit", "100000"]);
        // The server logs each command it runs, newest first, with the
        // client that sent it.
        let log = || -> Vec<String> {
            server
                .tmux_stdout(&["show-messages"])
                .lines()
                .filter(|l| l.contains("command: set-option") && l.contains(&session))
                .filter_map(|l| l.split(' ').nth(1).map(str::to_string))
                .collect()
        };
        let setup = log().len();
        let writers = || {
            let all = log();
            all[..all.len() - setup].to_vec()
        };

        refresh(&projects_dir, server.name()).unwrap();
        let first = writers();
        refresh(&projects_dir, server.name()).unwrap();

        assert!(first.len() > 1, "{first:?}");
        assert!(first.iter().all(|c| *c == first[0]), "{first:?}");
        assert_eq!(writers(), first);
    }

    fn main_scope(agents: Vec<AgentSnapshot>) -> Snapshot {
        let attention = attention::main_attention(&agents);
        Snapshot {
            version: attention::VERSION,
            projects: vec![attention::ProjectSnapshot {
                name: "app".into(),
                root: "/src/app".into(),
                skipped: None,
                main: Some(ScopeSnapshot {
                    session: "app/main".into(),
                    session_exists: true,
                    agents,
                    attention,
                    working: false,
                    background_since: None,
                    last_activity: None,
                }),
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
            }),
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

    #[test]
    fn main_asking_needs_attention_alerts_once_and_unarmed_never_alerts() {
        let asking = main_scope(vec![main_agent(
            AgentState::Asking,
            WaitingKind::Plan,
            "plan approval",
        )]);
        let published = |attention: &str| Options {
            clients: vec![Client::named("c1")],
            sessions: vec![Holder::session(
                "app/main",
                &[(PROJECT, "app"), (ATTENTION, attention)],
            )],
            ..Options::default()
        };
        let now = Utc::now();

        let commands = commands(&asking, &published(""), now);
        assert_eq!(sets(&commands, ATTENTION), ["asking"]);
        assert_eq!(sets(&commands, REASON), ["main: plan approval"]);
        assert_eq!(sets(&commands, BADGE), ["#[fg=red,bold]\u{f059}#[default]"]);
        assert_eq!(sets(&commands, COUNT), ["1"]);
        assert_eq!(
            announced(&commands),
            ["pm: app/main asking: main: plan approval"]
        );
        assert!(announced(&super::commands(&asking, &published("asking"), now)).is_empty());

        let unarmed = main_scope(vec![main_agent(
            AgentState::Unarmed,
            WaitingKind::Interrupted,
            "interrupted",
        )]);
        let commands = super::commands(&unarmed, &published(""), now);
        assert_eq!(sets(&commands, ATTENTION), ["unarmed"]);
        assert_eq!(
            sets(&commands, SUMMARY),
            ["#[fg=magenta]\u{f1f6} 1 unarmed#[default]"]
        );
        assert!(announced(&commands).is_empty());
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

    /// What pm's announcement shows on the status line of `window`.
    fn shown(server: &OwnServer, window: &str) -> String {
        server.tmux_stdout(&[
            "display",
            "-p",
            "-t",
            window,
            "#{?@pm_announcement_hidden,#{@pm_announcement_window},#{@pm_announcement}}",
        ])
    }

    #[test]
    fn an_announcement_shows_everywhere_but_an_ask_not_on_the_asking_agents_window() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("announce");
        Announcements::held(&server);
        published_session(&server, dir.path(), "app/login");
        published_session(&server, dir.path(), "app/main");
        let asking = tmux::new_window(server.name(), "app/main", dir.path(), None, true).unwrap();
        let elsewhere = "app/login:0";
        let mut agent = main_agent(AgentState::Asking, WaitingKind::Plan, "50% of #1 %H");
        agent.window = Some(asking.clone());
        agent.pane = Some(server.tmux_stdout(&["display", "-p", "-t", &asking, "#{pane_id}"]));
        let snapshot = Snapshot {
            features: ready_feature(false).features,
            ..main_scope(vec![agent])
        };
        publish(&server, &snapshot);

        let ready = "app/login ready: Adds login";
        assert_eq!(
            shown(&server, elsewhere),
            format!("pm: {ready} · app/main asking: main: 50% of ##1 %H"),
            "a # escaped for the status line, a % left alone"
        );
        assert_eq!(shown(&server, &asking), format!("pm: {ready}"));

        let held = publish_announcement(&server, &[later("later")]);
        assert_eq!(
            [shown(&server, elsewhere), shown(&server, &asking)],
            ["pm: later", "pm: later"],
            "the next announcement shows on the asking window too"
        );
        assert!(
            held.windows
                .iter()
                .all(|w| w.get(ANNOUNCEMENT_HIDDEN).is_empty())
        );
    }

    fn later(text: &str) -> Alert<'static> {
        Alert {
            text: text.into(),
            pane: None,
        }
    }

    /// Announce `alerts` on `server`, returning what is published after.
    fn publish_announcement(server: &OwnServer, alerts: &[Alert]) -> Options {
        let read = || {
            options::read(server.name(), &[], WINDOW_READ, GLOBAL_READ)
                .unwrap()
                .unwrap()
        };
        options::run(server.name(), &announce(alerts, &read(), Utc::now())).unwrap();
        read()
    }

    #[test]
    fn an_announcement_is_cleared_after_the_display_time_unless_a_newer_one_replaced_it() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("announce-expiry");
        tmux::create_session(server.name(), "app/main", dir.path()).unwrap();
        let window = "app/main:0";
        let asking = tmux::new_window(server.name(), "app/main", dir.path(), None, true).unwrap();
        let pane = server.tmux_stdout(&["display", "-p", "-t", &asking, "#{pane_id}"]);
        server.tmux_stdout(&["set", "-g", DISPLAY_TIME, "1000"]);
        let wait = |ms| std::thread::sleep(std::time::Duration::from_millis(ms));

        publish_announcement(&server, &[later("first")]);
        wait(500);
        let ask = Alert {
            text: "asking".into(),
            pane: Some(&pane),
        };
        publish_announcement(&server, &[later("second"), ask]);
        wait(700);
        assert_eq!(
            [shown(&server, window), shown(&server, &asking)],
            ["pm: second · asking", "pm: second"],
            "the first one's timer has fired"
        );
        wait(800);
        assert_eq!([shown(&server, window), shown(&server, &asking)], ["", ""]);
        assert_eq!(
            server.tmux_stdout(&["show", "-wqv", "-t", &asking, ANNOUNCEMENT_HIDDEN]),
            ""
        );
    }

    fn ready_feature(busy: bool) -> Snapshot {
        feature_scope(Progress::Ready, busy, Vec::new())
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

    #[test]
    fn a_ready_feature_alerts_once_however_often_its_team_wakes() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("ready-busy");
        published_session(&server, dir.path(), "app/login");
        let mut announced = Announcements::held(&server);
        let mut badges = Vec::new();
        let mut attentions = Vec::new();
        let mut counts = Vec::new();
        for busy in [true, false, true, false, false] {
            let held = publish(&server, &ready_feature(busy));
            announced.take();
            let login = held
                .sessions
                .iter()
                .find(|s| s.target == "app/login")
                .unwrap();
            badges.push(login.get(BADGE).to_string());
            attentions.push(login.get(ATTENTION).to_string());
            counts.push(held.global.get(COUNT).to_string());
        }

        assert_eq!(announced.texts, ["pm: app/login ready: Adds login"]);
        let ready = "#[fg=green,bold]\u{f058}#[default]";
        assert_eq!(badges, [ready; 5]);
        assert_eq!(attentions, ["", "ready", "", "ready", "ready"]);
        assert_eq!(counts, ["0", "1", "0", "1", "1"]);
    }

    #[test]
    fn the_count_is_the_sessions_that_publish_an_attention() {
        let open = ready_feature(false).features.remove(0);
        let closed = FeatureSnapshot {
            name: "closed".into(),
            session: "app/closed".into(),
            session_exists: false,
            ..open.clone()
        };
        let snapshot = Snapshot {
            features: vec![open, closed],
            ..ready_feature(false)
        };
        let published = Options {
            sessions: vec![Holder::session("app/login", &[(PROJECT, "app")])],
            ..Options::default()
        };

        let commands = commands(&snapshot, &published, Utc::now());
        assert_eq!(sets(&commands, ATTENTION), ["ready"]);
        assert_eq!(sets(&commands, COUNT), ["1"]);
    }

    #[test]
    fn a_kind_alerts_once_per_episode_however_often_it_is_outranked() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("episode");
        published_session(&server, dir.path(), "app/login");
        let mut announced = Announcements::held(&server);
        let asking = || {
            vec![AgentSnapshot {
                waiting: Some(attention::WaitingSnapshot {
                    kind: WaitingKind::Permission,
                    detail: "permission".into(),
                    since: None,
                }),
                ..agent_in(AgentState::Asking)
            }]
        };
        let idle = || vec![agent_in(AgentState::Idle)];

        for (progress, agents) in [
            (Progress::Ready, idle()),
            (Progress::Ready, asking()),
            (Progress::Ready, idle()),
            (Progress::Wip, idle()),
            (Progress::Ready, idle()),
        ] {
            publish(&server, &feature_scope(progress, false, agents));
            announced.take();
        }

        assert_eq!(
            announced.texts,
            [
                "pm: app/login ready: Adds login",
                "pm: app/login asking: implementer: permission",
                "pm: app/login ready: Adds login",
            ]
        );
    }

    #[test]
    fn a_closed_feature_alerts_once_when_it_turns_ready_but_not_on_closing_or_a_new_server() {
        let closed = |progress: Progress| Snapshot {
            features: vec![FeatureSnapshot {
                session_exists: false,
                ..feature_scope(progress, false, Vec::new())
                    .features
                    .remove(0)
            }],
            ..ready_feature(false)
        };
        let refresh = |snapshot: &Snapshot, global: &[(&str, &str)], session: Option<Holder>| {
            let published = Options {
                clients: vec![Client::named("c1")],
                sessions: session.into_iter().collect(),
                global: Holder::session("", global),
                ..Options::default()
            };
            let commands = commands(snapshot, &published, Utc::now());
            let record = commands
                .iter()
                .find(|c| c[0] == "set-option" && c.iter().any(|a| a == FEATURES_ALERTED))
                .map(|c| c[c.len() - 2..].join(" "));
            (announced(&commands).len(), record.unwrap_or_default())
        };
        let served = (COUNT, "0");

        assert_eq!(
            refresh(&closed(Progress::Ready), &[served], None),
            (1, "@pm_features_alerted app/login=ready".into()),
            "turned ready while closed"
        );
        let alerted = (FEATURES_ALERTED, "app/login=ready");
        assert_eq!(
            refresh(&closed(Progress::Ready), &[served, alerted], None),
            (0, String::new())
        );
        assert_eq!(
            refresh(&closed(Progress::Wip), &[served, alerted], None),
            (0, "-u @pm_features_alerted".into()),
            "the episode ended"
        );
        assert_eq!(
            refresh(&closed(Progress::Blocked), &[], None),
            (0, "@pm_features_alerted app/login=blocked".into()),
            "a new server records what was set before it"
        );

        let open = Holder::session("app/login", &[(PROJECT, "app"), (ALERTED, "ready")]);
        let (_, recorded) = refresh(&ready_feature(false), &[served], Some(open));
        assert_eq!(
            recorded, "@pm_features_alerted app/login=ready",
            "an open feature's record"
        );
        assert_eq!(
            refresh(&closed(Progress::Ready), &[served, alerted], None).0,
            0,
            "closing a ready feature"
        );

        let mut two = closed(Progress::Ready);
        let spaced = FeatureSnapshot {
            project: "My App".into(),
            session: "My App/login".into(),
            ..two.features[0].clone()
        };
        two.features.push(spaced);
        let (alerts, both) = refresh(&two, &[served], None);
        assert_eq!(alerts, 1, "one alert naming both");
        let both = both
            .strip_prefix("@pm_features_alerted ")
            .unwrap()
            .to_string();
        assert_eq!(
            refresh(&two, &[served, (FEATURES_ALERTED, &both)], None),
            (0, String::new()),
            "a project name with a space keeps its record"
        );
    }

    fn agent_in(state: AgentState) -> AgentSnapshot {
        AgentSnapshot {
            name: "implementer".into(),
            state,
            unread: 0,
            window: Some("app/login:1".into()),
            pane: None,
            waiting: None,
        }
    }

    #[test]
    fn a_reopened_session_shows_a_standing_progress_without_alerting_it() {
        let opened = |snapshot: &Snapshot| {
            let published = Options {
                clients: vec![Client::named("c1")],
                sessions: vec![Holder::session("app/login", &[])],
                ..Options::default()
            };
            let commands = commands(snapshot, &published, Utc::now());
            (
                announced(&commands).len(),
                sets(&commands, ATTENTION).concat(),
            )
        };

        assert_eq!(opened(&ready_feature(false)), (0, "ready".into()));
        assert_eq!(
            opened(&feature_scope(Progress::Blocked, false, Vec::new())),
            (0, "blocked".into())
        );
        let asking = vec![AgentSnapshot {
            waiting: Some(attention::WaitingSnapshot {
                kind: WaitingKind::Permission,
                detail: "permission".into(),
                since: None,
            }),
            ..agent_in(AgentState::Asking)
        }];
        assert_eq!(
            opened(&feature_scope(Progress::Wip, false, asking)),
            (1, "asking".into()),
            "a dialog is up now"
        );
    }

    #[test]
    fn a_main_session_shows_work_once() {
        let now = Utc::now();
        let main = |lead: AgentState, other: AgentState, working, minutes_ago| {
            let mut snapshot = main_scope(vec![
                AgentSnapshot {
                    name: "main".into(),
                    state: lead,
                    unread: 0,
                    window: Some("app/main:1".into()),
                    pane: None,
                    waiting: None,
                },
                AgentSnapshot {
                    name: "helper".into(),
                    state: other,
                    unread: 0,
                    window: Some("app/main:2".into()),
                    pane: None,
                    waiting: None,
                },
            ]);
            let scope = snapshot.projects[0].main.as_mut().unwrap();
            scope.working = working;
            scope.last_activity = Some(now - chrono::Duration::minutes(minutes_ago));
            snapshot
        };
        let published = Options {
            sessions: vec![Holder::session("app/main", &[])],
            ..Options::default()
        };
        let activity = |snapshot: &Snapshot| {
            let commands = commands(snapshot, &published, now);
            [ACTIVITY, ACTIVITY_LABEL]
                .iter()
                .flat_map(|name| sets(&commands, name))
                .map(String::from)
                .collect::<Vec<_>>()
        };

        assert!(
            activity(&main(AgentState::Busy, AgentState::Idle, true, 1)).is_empty(),
            "the main agent's badge shows it"
        );
        assert_eq!(
            sets(
                &commands(
                    &main(AgentState::Busy, AgentState::Idle, true, 1),
                    &published,
                    now
                ),
                LABEL
            ),
            ["#[fg=green]\u{f013} busy#[default]"],
            "main's label is its main agent's state"
        );
        assert_eq!(
            activity(&main(AgentState::Idle, AgentState::Busy, true, 1)),
            [
                "#[fg=green]\u{f013}#[default]",
                "#[fg=green]\u{f013} working#[default]"
            ],
            "another agent of main's is working"
        );
        assert_eq!(
            activity(&main(AgentState::Idle, AgentState::Idle, false, 185)),
            [
                "#[fg=colour245]3h#[default]",
                "#[fg=colour245]quiet 3h#[default]"
            ]
        );
        let mut waiting = main(AgentState::Background, AgentState::Idle, false, 185);
        waiting.projects[0].main.as_mut().unwrap().background_since =
            Some(now - chrono::Duration::hours(26));
        assert_eq!(
            activity(&waiting),
            [
                "#[fg=green]\u{f110} 1d#[default]",
                "#[fg=green]\u{f110} background 1d#[default]"
            ],
            "the main agent's badge doesn't show how long"
        );
    }

    #[test]
    fn a_stalled_scope_carries_a_reason_though_its_attention_has_none() {
        let stalled = Attention {
            kind: AttentionKind::Stalled,
            detail: None,
            agent: None,
        };
        assert_eq!(reason(&stalled).as_deref(), Some(STALLED));
    }

    #[test]
    fn activity_is_drawn_with_its_glyph_and_age() {
        let now = Utc::now();
        let ago = |minutes| now - chrono::Duration::minutes(minutes);
        assert_eq!(
            drawn(Activity::Working, now),
            [
                "#[fg=green]\u{f013}#[default]",
                "#[fg=green]\u{f013} working#[default]"
            ]
        );
        assert_eq!(
            drawn(Activity::Background(ago(1500)), now),
            [
                "#[fg=green]\u{f110} 1d#[default]",
                "#[fg=green]\u{f110} background 1d#[default]"
            ]
        );
        assert_eq!(
            drawn(Activity::Quiet(ago(185)), now),
            [
                "#[fg=colour245]3h#[default]",
                "#[fg=colour245]quiet 3h#[default]"
            ]
        );
    }

    #[test]
    fn no_server_publishes_nothing() {
        let dir = tempdir().unwrap();
        let never = format!("pm-test-{}-never", std::process::id());
        refresh(dir.path(), Some(&never)).unwrap();
        assert!(!server_socket_exists(&never), "a refresh started a server");
    }

    #[test]
    fn a_project_whose_state_or_registry_entry_cannot_be_read_keeps_what_it_published() {
        let _serial = serial();
        let breakages: [fn(&Path, &Path, &str); 2] = [
            |project, _, _| {
                std::fs::write(paths::pm_dir(project).join("config.toml"), "not = [toml").unwrap()
            },
            |_, projects_dir, name| {
                std::fs::write(projects_dir.join(format!("{name}.toml")), "not = [toml").unwrap()
            },
        ];
        for breakage in breakages {
            let dir = tempdir().unwrap();
            let server = TestServer::new();
            let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
            let projects_dir = TestServer::registry_dir(&project);
            let session = tmux::session_name(&project_name, "login");
            feat_status(
                &project,
                "login",
                Progress::Blocked,
                Some("which DB?"),
                None,
            )
            .unwrap();
            refresh(&projects_dir, server.name()).unwrap();

            breakage(&project, &projects_dir, &project_name);
            refresh(&projects_dir, server.name()).unwrap();

            let now = published(&server);
            assert_eq!(
                values(&now.sessions, &session, &[PROJECT, ATTENTION, REASON]),
                [project_name.as_str(), "blocked", "which DB?"]
            );
            assert_eq!(now.global.get(COUNT), "0");
        }
    }

    #[test]
    fn a_client_gone_since_the_read_fails_no_refresh() {
        let _serial = serial();
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project);
        let session = tmux::session_name(&project_name, "login");
        refresh(&projects_dir, server.name()).unwrap();
        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            None,
        )
        .unwrap();

        let mut before = published(&server);
        before.clients.push(Client::named("client-gone"));
        let snapshot = attention::all(&projects_dir, server.name()).unwrap();
        write(server.name(), &commands(&snapshot, &before, Utc::now())).unwrap();

        let now = published(&server);
        assert_eq!(values(&now.sessions, &session, &[ATTENTION]), ["blocked"]);
    }
}
