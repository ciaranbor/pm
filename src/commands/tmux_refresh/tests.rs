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
    let shell = tmux::new_window(server.name(), &session, &project, Some("shell"), true).unwrap();
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
    let display =
        |target: &str, format: &str| server.tmux_stdout(&["display", "-p", "-t", target, format]);
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
    crate::commands::feat_new::feat_new(&crate::commands::feat_new::FeatNewParams::with_defaults(
        &project,
        &projects_dir,
        "search",
        server.name(),
    ))
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
        ["", "", "", "", "", "", "", "", "", ""]
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
    crate::commands::feat_new::feat_new(&crate::commands::feat_new::FeatNewParams::with_defaults(
        &project,
        &projects_dir,
        "login",
        server.name(),
    ))
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
            dialog: None,
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
    assert_eq!(badges, ["", ready, "", ready, ready]);
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
                dialog: None,
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
            dialog: None,
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
