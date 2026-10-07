use super::*;

#[test]
fn send_to_active_dead_window_queues_and_respawns() {
    // An agent flagged active (active = true) whose tmux window has died
    // should have its message queued AND its window healed.
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);

    create_agent_definition(&root, "reviewer");

    // Spawn agent (active = true), then kill its window (simulating crash).
    crate::commands::agent_spawn::agent_spawn(
        &root,
        &feature,
        "reviewer",
        None,
        None,
        server.name(),
    )
    .unwrap();

    // Kill and recreate session to remove the window.
    tmux::kill_session(server.name(), &session_name).unwrap();
    let worktree = root.join("login");
    tmux::create_session(server.name(), &session_name, &worktree).unwrap();

    let sent = agent_send(
        &root,
        &feature,
        None,
        "reviewer",
        "implementer",
        "review this",
        server.name(),
    )
    .unwrap();
    // Message queued and the dead window healed.
    assert!(sent.status.contains("Message 001 sent to 'reviewer'"));
    let heal = sent.heal.unwrap();
    assert_eq!(
        (heal.scope.as_str(), heal.agent.as_str()),
        ("login", "reviewer")
    );
    assert!(heal.report.contains("Spawned agent 'reviewer'"));

    // The window now exists again.
    assert!(
        tmux::find_window(server.name(), &session_name, "reviewer")
            .unwrap()
            .is_some()
    );
}

/// The agent pane of `agent`'s window, and the windows of its session.
fn agent_pane_and_windows(server: &TestServer, session: &str, agent: &str) -> (String, String) {
    let window = tmux::find_window(server.name(), session, agent)
        .unwrap()
        .unwrap();
    let pane = tmux::panes::agent_pane(server.name(), &window)
        .unwrap()
        .unwrap();
    let windows = server.tmux_stdout(&["list-windows", "-t", session, "-F", "#{window_name}"]);
    (pane, windows)
}

#[test]
fn send_to_agent_whose_harness_exited_relaunches_it_in_its_pane() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);
    create_agent_definition(&root, "reviewer");
    server.spawn_dead_fake_agent(&root, &session_name, &feature, "reviewer");
    let before = agent_pane_and_windows(&server, &session_name, "reviewer");

    let sent = agent_send(
        &root,
        &feature,
        None,
        "reviewer",
        "implementer",
        "review this",
        server.name(),
    )
    .unwrap();

    let heal = sent.heal.unwrap();
    assert!(
        heal.report
            .contains(&format!("agent 'reviewer' in {session_name}:")),
        "{}",
        heal.report
    );
    assert_eq!(
        agent_pane_and_windows(&server, &session_name, "reviewer"),
        before
    );
    let launched = || {
        server
            .tmux_stdout(&["capture-pane", "-p", "-t", &before.0])
            .contains("PM_AGENT_NAME=reviewer")
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !launched() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(launched());
}

#[test]
fn send_leaves_an_exited_harness_alone_while_its_pane_is_watched() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);
    create_agent_definition(&root, "reviewer");
    let target = server.spawn_dead_fake_agent(&root, &session_name, &feature, "reviewer");
    server.tmux_stdout(&["select-window", "-t", &target]);
    let _viewing = crate::testing::ControlClient::attach(server.name(), &session_name);

    let sent = agent_send(
        &root,
        &feature,
        None,
        "reviewer",
        "implementer",
        "review this",
        server.name(),
    )
    .unwrap();

    assert!(sent.heal.is_none(), "{sent:?}");
    assert!(
        !server
            .tmux_stdout(&["capture-pane", "-p", "-t", &target])
            .contains("PM_AGENT_NAME")
    );
}

#[test]
fn send_leaves_an_agent_whose_harness_is_still_starting_alone() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);
    create_agent_definition(&root, "reviewer");
    let target = server.spawn_dead_fake_agent(&root, &session_name, &feature, "reviewer");
    let startup =
        crate::state::runtime::Waiting::now(crate::state::runtime::WaitingKind::Startup, None);
    crate::state::runtime::write_waiting(&root, &feature, "reviewer", &startup).unwrap();

    let sent = agent_send(
        &root,
        &feature,
        None,
        "reviewer",
        "implementer",
        "review this",
        server.name(),
    )
    .unwrap();

    assert!(sent.heal.is_none(), "{sent:?}");
    assert!(
        !server
            .tmux_stdout(&["capture-pane", "-p", "-t", &target])
            .contains("PM_AGENT_NAME")
    );
}

#[test]
fn send_delivers_when_heal_fails() {
    // The heal runs after the message is durably queued, so when tmux is
    // unreachable (the sandboxed-agent case) the send still succeeds and
    // the message is in the inbox.
    let dir = tempdir().unwrap();
    let root = setup_project_files(dir.path(), "proj");
    create_agent_definition(&root, "reviewer");

    let agents_dir = paths::agents_dir(&root);
    let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
    registry.register(
        "reviewer",
        AgentEntry {
            agent_type: AgentType::Agent,
            session_id: String::new(),
            window_name: "reviewer".to_string(),
            active: true,
            agent_definition: None,
            harness: Default::default(),
            spawned_at: None,
        },
    );
    registry.save(&agents_dir, "login").unwrap();

    // No tmux server exists under this socket name, so every tmux call
    // in the heal fails the way an unreachable socket does.
    let bogus_server = format!("pm-test-no-server-{}", std::process::id());

    let status = agent_send(
        &root,
        "login",
        None,
        "reviewer",
        "implementer",
        "heal-test message",
        Some(&bogus_server),
    )
    .unwrap()
    .status;
    assert_eq!(
        status,
        "Message 001 sent to 'reviewer' (from 'implementer')"
    );

    let messages_dir = paths::messages_dir(&root);
    let msg = messages::read_at(&messages_dir, "login", "reviewer", "implementer", 1)
        .unwrap()
        .unwrap();
    assert_eq!(msg.body.trim(), "heal-test message");
}
