use super::*;

#[test]
fn options_are_published_and_cleared_once_their_value_goes_away() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, project_name) =
        server.setup_project_with_feature(dir.path(), "login");
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
        crate::testing::NO_SERVER,
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
            "",
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
            "",
        ]
    );
    assert_eq!(
        values(&now.windows, &shell, WINDOW_OPTIONS),
        ["", "", "", "mine", "", ""],
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

    feat_status(
        &project,
        "login",
        Progress::Wip,
        None,
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
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
            "",
        ]
    );
    assert_eq!(
        values(&now.windows, &reviewer, WINDOW_OPTIONS),
        ["", "", "", "", "", ""],
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
fn a_feature_that_goes_takes_its_session_options_with_it() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, project_name) =
        server.setup_project_with_feature(dir.path(), "login");
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
    let (project, projects_dir, project_name) =
        server.setup_project_with_feature(dir.path(), "login");
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
        ["", "", "", "", "", ""]
    );
}

#[test]
fn changes_are_written_in_one_tmux_call_and_a_repeat_writes_nothing() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, project_name) =
        server.setup_project_with_feature(dir.path(), "login");
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
