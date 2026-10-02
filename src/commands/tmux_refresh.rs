//! `pm tmux refresh`: the [`attention`] snapshot of every project, published
//! on the tmux server as `@pm_*` user options for status lines and tree
//! formats to read (README, "tmux integration", has the contract).
//!
//! It runs on every watcher tick ([`tmux_watch`](super::tmux_watch)) and
//! every push ([`tmux_push`](super::tmux_push)), so it costs the snapshot's
//! own reads, one `tmux` call that takes what is published now along with the
//! attached clients, and, only when something changed, one more that writes
//! the difference. What it last published is also the previous state a
//! transition alert is judged against; pm keeps no other record of it, so
//! refreshes of one server take turns on a lock keyed by the server's socket,
//! which names it however it was reached.
//!
//! A scope's agents, main's included, are found through the registry, never
//! by window, and only their windows carry options. A main session carries
//! `@pm_project` so its windows count as the project's when they are
//! cleared, and its main agent's badge, so the tree shows whether the
//! orchestrator is working or waiting on the user. Options are cleared only
//! on sessions of projects the snapshot read: a project whose state couldn't
//! be read keeps what it last published.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::paths;
use crate::tmux;
use crate::tmux::options::{self, Command, Holder, Options, Scope, format_text};

use chrono::{DateTime, Utc};

use super::attention::{
    self, AgentSnapshot, AgentState, Attention, AttentionKind, FeatureSnapshot, ScopeSnapshot,
    Snapshot,
};
use super::feat_status_view::span;

const PROJECT: &str = "@pm_project";
const FEATURE: &str = "@pm_feature";
const PROGRESS: &str = "@pm_progress";
const REASON: &str = "@pm_reason";
const ATTENTION: &str = "@pm_attention";
const BADGE: &str = "@pm_badge";
const ACTIVITY: &str = "@pm_activity";
const SESSION_OPTIONS: &[&str] = &[
    PROJECT, FEATURE, PROGRESS, REASON, ATTENTION, BADGE, ACTIVITY,
];

const AGENT: &str = "@pm_agent";
const AGENT_STATE: &str = "@pm_agent_state";
const UNREAD: &str = "@pm_unread";
const AGENT_BADGE: &str = "@pm_agent_badge";
pub(super) const WINDOW_OPTIONS: &[&str] = &[AGENT, AGENT_STATE, UNREAD, AGENT_BADGE];

const SUMMARY: &str = "@pm_summary";
const COUNT: &str = "@pm_count";
const GLOBAL_OPTIONS: &[&str] = &[SUMMARY, COUNT];

/// Publish the snapshot of every project registered in `projects_dir`. No
/// server running publishes nothing.
pub fn refresh(projects_dir: &Path, tmux_server: Option<&str>) -> Result<()> {
    let Some(socket) = tmux::socket_path(tmux_server)? else {
        return Ok(());
    };
    let lock = lock_file(&socket, "refresh")?;
    lock.lock()?;
    let Some(published) =
        options::read(tmux_server, SESSION_OPTIONS, WINDOW_OPTIONS, GLOBAL_OPTIONS)?
    else {
        return Ok(());
    };
    let snapshot = attention::all(projects_dir, tmux_server)?;
    write(tmux_server, &commands(&snapshot, &published, Utc::now()))
}

/// The file whose lock of `kind` stands for the server at `socket`.
pub(super) fn lock_file(socket: &str, kind: &str) -> Result<File> {
    let dir = paths::global_config_dir()?.join("tmux");
    std::fs::create_dir_all(&dir)?;
    let name: String = socket
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{name}.{kind}.lock")))?)
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
/// options, then an alert and a redraw for each client.
fn commands(snapshot: &Snapshot, published: &Options, now: DateTime<Utc>) -> Vec<Command> {
    let mut writes = Vec::new();
    let mut alerts = Vec::new();
    let mut sessions: HashSet<&str> = HashSet::new();
    let mut windows: HashSet<&str> = HashSet::new();
    for feature in &snapshot.features {
        // A session made since the read is published next time.
        let Some(held) = published
            .sessions
            .iter()
            .find(|s| s.target == feature.session)
        else {
            continue;
        };
        sessions.insert(&feature.session);
        let scope = Scope::Session(&feature.session);
        diff(&mut writes, scope, held, &session_values(feature, now));
        if matches!(
            feature.attention.kind,
            AttentionKind::Blocked | AttentionKind::Ready | AttentionKind::Asking
        ) && held.get(ATTENTION) != feature.attention.kind.to_string()
        {
            alerts.push(alert(&feature.session, &feature.attention));
        }
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
        diff(
            &mut writes,
            Scope::Session(&main.session),
            held,
            &main_values(&project.name, main, now),
        );
        if main.attention.kind == AttentionKind::Asking
            && held.get(ATTENTION) != main.attention.kind.to_string()
        {
            alerts.push(alert(&main.session, &main.attention));
        }
        agent_windows(&mut writes, &mut windows, published, &main.agents);
    }

    let read: HashSet<&str> = snapshot
        .projects
        .iter()
        .filter(|p| p.skipped.is_none())
        .map(|p| p.name.as_str())
        .collect();
    let ours = |session: &str| {
        published
            .sessions
            .iter()
            .any(|s| s.target == session && read.contains(s.get(PROJECT)))
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
        &global_values(snapshot),
    );

    if !alerts.is_empty() {
        let text = format!("pm: {}", alerts.join(" · "));
        writes.extend(published.clients.iter().map(|c| options::display(c, &text)));
    }
    if !writes.is_empty() {
        writes.extend(published.clients.iter().map(|c| options::refresh_status(c)));
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

fn session_values(
    feature: &FeatureSnapshot,
    now: DateTime<Utc>,
) -> Vec<(&'static str, Option<String>)> {
    let kind = feature.attention.kind;
    let needs = (kind != AttentionKind::None).then_some(kind);
    vec![
        (PROJECT, Some(format_text(&feature.project))),
        (FEATURE, Some(format_text(&feature.name))),
        (PROGRESS, Some(feature.progress.to_string())),
        (REASON, reason(&feature.attention)),
        (ATTENTION, needs.map(|k| k.to_string())),
        (
            BADGE,
            needs.map(|k| styled(attention_style(k), &k.to_string())),
        ),
        (
            ACTIVITY,
            activity(feature.working, feature.last_activity, now),
        ),
    ]
}

fn reason(attention: &Attention) -> Option<String> {
    attention
        .detail
        .as_deref()
        .map(format_text)
        .filter(|r| !r.is_empty())
}

/// A main session has no feature or progress. Its badge is its main
/// agent's, whatever its attention.
fn main_values(
    project: &str,
    main: &ScopeSnapshot,
    now: DateTime<Utc>,
) -> Vec<(&'static str, Option<String>)> {
    let kind = main.attention.kind;
    let lead = main
        .agents
        .iter()
        .find(|a| a.name == "main")
        .or(main.agents.first());
    vec![
        (PROJECT, Some(format_text(project))),
        (FEATURE, None),
        (PROGRESS, None),
        (REASON, reason(&main.attention)),
        (
            ATTENTION,
            (kind != AttentionKind::None).then(|| kind.to_string()),
        ),
        (BADGE, lead.map(|a| agent_badge(a.state, a.unread))),
        (ACTIVITY, activity(main.working, main.last_activity, now)),
    ]
}

/// The busy glyph while the scope works, else how long it has been quiet,
/// once that is long enough to matter.
fn activity(
    working: bool,
    last_activity: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<String> {
    if working {
        return Some(styled(
            agent_style(AgentState::Busy),
            agent_glyph(AgentState::Busy),
        ));
    }
    let since = attention::quiet_since(working, last_activity, now)?;
    Some(styled("fg=colour245", &span(since, now)))
}

pub(super) fn window_values(agent: &AgentSnapshot) -> Vec<(&'static str, Option<String>)> {
    vec![
        (AGENT, Some(format_text(&agent.name))),
        (AGENT_STATE, Some(agent.state.to_string())),
        (UNREAD, Some(agent.unread.to_string())),
        (AGENT_BADGE, Some(agent_badge(agent.state, agent.unread))),
    ]
}

/// Colours only the foreground, so the glyphs sit on the surrounding
/// background, then resets to the window's base style once at the end.
fn agent_badge(state: AgentState, unread: u32) -> String {
    let mut badge = format!("#[{}]{}", agent_style(state), agent_glyph(state));
    if unread > 0 {
        // nf-fa-envelope
        badge.push_str("#[fg=yellow]\u{f0e0}");
    }
    badge + "#[default]"
}

fn global_values(snapshot: &Snapshot) -> Vec<(&'static str, Option<String>)> {
    let mains = snapshot
        .projects
        .iter()
        .filter_map(|p| p.main.as_ref())
        .map(|m| m.attention.kind);
    let mut kinds: Vec<AttentionKind> = snapshot
        .features
        .iter()
        .map(|f| f.attention.kind)
        .chain(mains)
        .filter(|k| *k != AttentionKind::None)
        .collect();
    kinds.sort();
    let summary: Vec<String> = kinds
        .chunk_by(|a, b| a == b)
        .map(|run| {
            styled(
                attention_style(run[0]),
                &format!("{} {}", run.len(), run[0]),
            )
        })
        .collect();
    vec![
        (SUMMARY, Some(summary.join(" · ")).filter(|s| !s.is_empty())),
        (COUNT, Some(kinds.len().to_string())),
    ]
}

/// `text` in `style`, then back to the surrounding style. Named and
/// 256-palette colours only, which every tmux release draws.
fn styled(style: &str, text: &str) -> String {
    format!("#[{style}]{text}#[default]")
}

fn attention_style(kind: AttentionKind) -> &'static str {
    match kind {
        AttentionKind::Blocked | AttentionKind::Asking => "fg=red,bold",
        AttentionKind::Cleanup | AttentionKind::Unarmed => "fg=magenta",
        AttentionKind::Ready => "fg=green,bold",
        AttentionKind::Dead => "fg=red",
        AttentionKind::Stalled => "fg=yellow",
        AttentionKind::None => "default",
    }
}

fn agent_style(state: AgentState) -> &'static str {
    match state {
        AgentState::Busy | AgentState::Background => "fg=green",
        AgentState::Asking => "fg=red,bold",
        AgentState::Unarmed => "fg=magenta",
        AgentState::Dead => "fg=red",
        AgentState::Idle | AgentState::Stopped | AgentState::Closed => "fg=colour245",
    }
}

/// Nerd Font (v3) glyphs, one cell wide: nf-fa-gear, nf-fa-question_circle,
/// nf-fa-bell_slash, nf-fa-spinner, nf-fa-hourglass_half, nf-md-skull,
/// nf-fa-stop.
fn agent_glyph(state: AgentState) -> &'static str {
    match state {
        AgentState::Busy => "\u{f013}",
        AgentState::Asking => "\u{f059}",
        AgentState::Unarmed => "\u{f1f6}",
        AgentState::Background => "\u{f110}",
        AgentState::Idle => "\u{f252}",
        AgentState::Dead => "\u{f068c}",
        AgentState::Stopped | AgentState::Closed => "\u{f04d}",
    }
}

fn alert(session: &str, attention: &Attention) -> String {
    let what = format!("{session} {}", attention.kind);
    match &attention.detail {
        Some(detail) => format!("{what}: {detail}"),
        None => what,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_delete::feat_delete, feat_status::feat_status};
    use crate::messages;
    use crate::state::agent::AgentRegistry;
    use crate::state::feature::{FeatureState, FeatureStatus, Progress};
    use crate::state::paths;
    use crate::state::runtime::WaitingKind;
    use crate::testing::{OwnServer, TestServer, server_socket_exists};
    use crate::tmux;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, ChildStdin, Stdio};
    use std::sync::{Arc, Mutex, MutexGuard};
    use tempfile::tempdir;

    /// A refresh writes server-wide options and alerts every client, so
    /// these tests take turns on the shared server.
    fn serial() -> MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn published(server: &TestServer) -> Options {
        options::read(
            server.name(),
            SESSION_OPTIONS,
            WINDOW_OPTIONS,
            GLOBAL_OPTIONS,
        )
        .unwrap()
        .unwrap()
    }

    fn values<'a>(holders: &'a [Holder], target: &str, names: &[&str]) -> Vec<&'a str> {
        let holder = holders.iter().find(|h| h.target == target).unwrap();
        names.iter().map(|n| holder.get(n)).collect()
    }

    fn tmux_out(server: &TestServer, args: &[&str]) -> String {
        let out = std::process::Command::new("tmux")
            .args(["-L", server.name().unwrap()])
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A control-mode client attached to a session, recording what tmux
    /// sends it.
    struct ControlClient {
        child: Child,
        stdin: ChildStdin,
        output: Arc<Mutex<Vec<String>>>,
        syncs: u32,
    }

    impl ControlClient {
        fn attach(server: Option<&str>, session: &str) -> Self {
            let mut child = std::process::Command::new("tmux")
                .args(["-L", server.unwrap(), "-C", "attach", "-t"])
                .arg(format!("={session}"))
                .env_remove("TMUX")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let output = Arc::new(Mutex::new(Vec::new()));
            let lines = Arc::clone(&output);
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
                    lines.lock().unwrap().push(line);
                }
            });
            let mut client = Self {
                child,
                stdin,
                output,
                syncs: 0,
            };
            client.sync();
            client
        }

        /// Wait until tmux has answered a command sent now, so whatever it
        /// sent before has arrived.
        fn sync(&mut self) {
            self.syncs += 1;
            let marker = format!("sync-{}", self.syncs);
            writeln!(self.stdin, "display-message {marker}").unwrap();
            for _ in 0..500 {
                if self.messages_raw().contains(&marker) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!(
                "control client never saw {marker}: {:?}",
                self.output.lock().unwrap()
            );
        }

        fn messages_raw(&self) -> Vec<String> {
            self.output
                .lock()
                .unwrap()
                .iter()
                .filter_map(|l| l.strip_prefix("%message "))
                .map(str::to_string)
                .collect()
        }

        /// The status-line messages tmux has shown the client.
        fn messages(&mut self) -> Vec<String> {
            self.sync();
            self.messages_raw()
                .into_iter()
                .filter(|m| !m.starts_with("sync-"))
                .collect()
        }
    }

    impl Drop for ControlClient {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
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
                "#[fg=red,bold]blocked#[default]",
                "",
            ]
        );
        assert_eq!(
            values(&now.windows, &implementer, WINDOW_OPTIONS),
            [
                "implementer",
                "idle",
                "0",
                "#[fg=colour245]\u{f252}#[default]"
            ]
        );
        assert_eq!(
            values(&now.windows, &reviewer, WINDOW_OPTIONS),
            [
                "reviewer",
                "busy",
                "1",
                "#[fg=green]\u{f013}#[fg=yellow]\u{f0e0}#[default]"
            ]
        );
        assert_eq!(
            values(&now.windows, &shell, WINDOW_OPTIONS),
            ["", "", "", "mine"],
            "a window that is no agent's"
        );
        assert_eq!(
            [now.global.get(COUNT), now.global.get(SUMMARY)],
            ["1", "#[fg=red,bold]1 blocked#[default]"]
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
            [project_name.as_str(), "login", "wip", "", "", "", ""]
        );
        assert_eq!(
            values(&now.windows, &implementer, WINDOW_OPTIONS),
            [
                "implementer",
                "stopped",
                "0",
                "#[fg=colour245]\u{f04d}#[default]"
            ]
        );
        assert_eq!(
            values(&now.windows, &reviewer, WINDOW_OPTIONS),
            ["", "", "", ""],
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
                "#[fg=colour245]\u{f252}#[fg=yellow]\u{f0e0}#[default]"
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
                "#[fg=colour245]\u{f252}#[fg=yellow]\u{f0e0}#[default]",
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
            ["", "", "", ""]
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
            ["1", "#[fg=magenta]1 cleanup#[default]"]
        );

        feat_delete(&project, &projects_dir, "search", true, server.name()).unwrap();
        // State gone, session left open.
        std::fs::remove_file(features_dir.join("login.toml")).unwrap();
        refresh(&projects_dir, server.name()).unwrap();

        let now = published(&server);
        assert!(!now.sessions.iter().any(|s| s.target == search));
        assert_eq!(
            values(&now.sessions, &login, SESSION_OPTIONS),
            ["", "", "", "", "", "", ""]
        );
        assert_eq!([now.global.get(COUNT), now.global.get(SUMMARY)], ["0", ""]);
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
        let mut client = ControlClient::attach(server.name(), &session);

        refresh(&projects_dir, server.name()).unwrap();
        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            None,
        )
        .unwrap();
        refresh(&projects_dir, server.name()).unwrap();
        refresh(&projects_dir, server.name()).unwrap();
        let summary = paths::summary_path(&project, "login");
        std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
        std::fs::write(&summary, "Adds login\n").unwrap();
        feat_status(&project, "login", Progress::Ready, None, None).unwrap();
        refresh(&projects_dir, server.name()).unwrap();
        refresh(&projects_dir, server.name()).unwrap();

        assert_eq!(
            client.messages(),
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
        tmux_out(&server, &["set-option", "-s", "message-limit", "100000"]);
        // The server logs each command it runs with the client that sent it.
        let writers = || -> Vec<String> {
            tmux_out(&server, &["show-messages"])
                .lines()
                .filter(|l| l.contains("command: set-option") && l.contains(&session))
                .filter_map(|l| l.split(' ').nth(1).map(str::to_string))
                .collect()
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
            waiting: Some(attention::WaitingSnapshot {
                kind,
                detail: detail.into(),
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

    fn displayed(commands: &[Command]) -> Vec<&str> {
        commands
            .iter()
            .filter(|c| c[0] == "display-message")
            .map(|c| c[3].as_str())
            .collect()
    }

    #[test]
    fn main_asking_needs_attention_alerts_once_and_unarmed_never_alerts() {
        let asking = main_scope(vec![main_agent(
            AgentState::Asking,
            WaitingKind::Plan,
            "plan approval",
        )]);
        let published = |attention: &str| Options {
            clients: vec!["c1".into()],
            sessions: vec![Holder::session("app/main", &[(ATTENTION, attention)])],
            ..Options::default()
        };
        let now = Utc::now();

        let commands = commands(&asking, &published(""), now);
        assert_eq!(sets(&commands, ATTENTION), ["asking"]);
        assert_eq!(sets(&commands, REASON), ["main: plan approval"]);
        assert_eq!(sets(&commands, BADGE), ["#[fg=red,bold]\u{f059}#[default]"]);
        assert_eq!(sets(&commands, COUNT), ["1"]);
        assert_eq!(
            displayed(&commands),
            ["pm: app/main asking: main: plan approval"]
        );
        assert!(displayed(&super::commands(&asking, &published("asking"), now)).is_empty());

        let unarmed = main_scope(vec![main_agent(
            AgentState::Unarmed,
            WaitingKind::Interrupted,
            "interrupted",
        )]);
        let commands = super::commands(&unarmed, &published(""), now);
        assert_eq!(sets(&commands, ATTENTION), ["unarmed"]);
        assert_eq!(
            sets(&commands, SUMMARY),
            ["#[fg=magenta]1 unarmed#[default]"]
        );
        assert!(displayed(&commands).is_empty());
    }

    #[test]
    fn activity_shows_work_or_a_quiet_spell_long_enough_to_matter() {
        let now = Utc::now();
        let ago = |minutes| Some(now - chrono::Duration::minutes(minutes));
        assert_eq!(
            activity(true, ago(1), now).as_deref(),
            Some("#[fg=green]\u{f013}#[default]")
        );
        assert_eq!(activity(false, ago(9), now), None, "between turns");
        assert_eq!(
            activity(false, ago(185), now).as_deref(),
            Some("#[fg=colour245]3h#[default]")
        );
        assert_eq!(activity(false, None, now), None);
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
        before.clients.push("client-gone".into());
        let snapshot = attention::all(&projects_dir, server.name()).unwrap();
        write(server.name(), &commands(&snapshot, &before, Utc::now())).unwrap();

        let now = published(&server);
        assert_eq!(values(&now.sessions, &session, &[ATTENTION]), ["blocked"]);
    }
}
