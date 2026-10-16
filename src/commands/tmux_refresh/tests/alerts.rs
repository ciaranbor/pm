use super::*;

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
        crate::testing::NO_SERVER,
    )
    .unwrap();
    refresh(&projects_dir, server.name()).unwrap();
    announced.take();
    refresh(&projects_dir, server.name()).unwrap();
    announced.take();
    let summary = paths::summary_path(&project, "login");
    std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
    std::fs::write(&summary, "Adds login\n").unwrap();
    feat_status(
        &project,
        "login",
        Progress::Ready,
        None,
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
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
