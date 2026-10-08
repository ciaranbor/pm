use super::*;

#[test]
fn send_increments_index_in_output() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);

    // Messaging never spawns, so the recipient needs a live window.
    create_agent_definition(&root, "reviewer");
    server.spawn_fake_agent(&root, &session_name, &feature, "reviewer");

    agent_send(
        &root,
        &feature,
        None,
        "reviewer",
        "implementer",
        "first",
        server.name(),
    )
    .unwrap();
    let msg = agent_send(
        &root,
        &feature,
        None,
        "reviewer",
        "implementer",
        "second",
        server.name(),
    )
    .unwrap()
    .status;
    assert!(msg.contains("Message 002"));
}

#[test]
fn send_cross_scope_shows_scopes_in_output() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, _session_name, _feature) = setup_project_with_tmux(dir.path(), &server);

    // Create a "main" scope setup with tmux session and agent definition
    let pm_dir = root.join(".pm");
    let config = ProjectConfig::load(&pm_dir).unwrap();
    let main_worktree = paths::main_worktree(&root);
    std::fs::create_dir_all(&main_worktree).unwrap();
    let main_session = tmux::session_name(&config.project.name, "main");
    tmux::create_session(server.name(), &main_session, &main_worktree).unwrap();

    // Agent definition in main worktree, and pre-spawn it active —
    // cross-scope sends require the target's agent to already be running.
    create_agent_definition(&root, "implementer");
    server.spawn_fake_agent(&root, &main_session, "main", "implementer");

    // Send from "login" scope to "main" scope
    let msg = agent_send(
        &root,
        "login",
        Some("main"),
        "implementer",
        "reviewer",
        "please look at this",
        server.name(),
    )
    .unwrap()
    .status;

    // Output should show cross-scope notation
    assert!(msg.contains("implementer@main"));
    assert!(msg.contains("reviewer@login"));
}

#[test]
fn send_same_scope_does_not_record_sender_scope() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);

    create_agent_definition(&root, "reviewer");
    server.spawn_fake_agent(&root, &session_name, &feature, "reviewer");

    agent_send(
        &root,
        &feature,
        None,
        "reviewer",
        "implementer",
        "hello",
        server.name(),
    )
    .unwrap();

    let messages_dir = paths::messages_dir(&root);
    let msg = messages::read_at(&messages_dir, &feature, "reviewer", "implementer", 1)
        .unwrap()
        .unwrap();
    assert_eq!(msg.meta.sender_scope, None);
}

#[test]
fn send_cross_scope_records_sender_scope_in_metadata() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, _session_name, _feature) = setup_project_with_tmux(dir.path(), &server);

    // Set up "main" scope with tmux session
    let pm_dir = root.join(".pm");
    let config = ProjectConfig::load(&pm_dir).unwrap();
    let main_worktree = paths::main_worktree(&root);
    std::fs::create_dir_all(&main_worktree).unwrap();
    let main_session = tmux::session_name(&config.project.name, "main");
    tmux::create_session(server.name(), &main_session, &main_worktree).unwrap();

    create_agent_definition(&root, "implementer");
    server.spawn_fake_agent(&root, &main_session, "main", "implementer");

    // Cross-scope: login → main
    agent_send(
        &root,
        "login",
        Some("main"),
        "implementer",
        "reviewer",
        "cross-scope msg",
        server.name(),
    )
    .unwrap();

    let messages_dir = paths::messages_dir(&root);
    let msg = messages::read_at(&messages_dir, "main", "implementer", "reviewer", 1)
        .unwrap()
        .unwrap();
    assert_eq!(msg.meta.sender_scope.as_deref(), Some("login"));
}

#[test]
fn send_cross_scope_dead_window_queues_and_respawns() {
    // Cross-scope sends go through the same `agent_send` heal path as
    // same-scope: an active recipient in the target scope whose window
    // has died gets its message queued AND its window healed.
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, _session_name, _feature) = setup_project_with_tmux(dir.path(), &server);

    // Set up "main" scope with tmux session + state.
    let pm_dir = root.join(".pm");
    let config = ProjectConfig::load(&pm_dir).unwrap();
    let main_worktree = paths::main_worktree(&root);
    std::fs::create_dir_all(&main_worktree).unwrap();
    let main_session = tmux::session_name(&config.project.name, "main");
    tmux::create_session(server.name(), &main_session, &main_worktree).unwrap();

    create_agent_definition(&root, "implementer");

    // Spawn the recipient active in main scope, then kill its window by
    // recreating the session (simulating a crash).
    crate::commands::agent_spawn::agent_spawn(
        &root,
        "main",
        "implementer",
        None,
        None,
        server.name(),
    )
    .unwrap();
    tmux::kill_session(server.name(), &main_session).unwrap();
    tmux::create_session(server.name(), &main_session, &main_worktree).unwrap();

    // Cross-scope: login → main, recipient active but window dead.
    let sent = agent_send(
        &root,
        "login",
        Some("main"),
        "implementer",
        "reviewer",
        "cross-scope heal",
        server.name(),
    )
    .unwrap();

    // Queued and healed.
    assert!(sent.status.contains("implementer@main"));
    let heal = sent.heal.unwrap();
    assert_eq!(
        (heal.scope.as_str(), heal.agent.as_str()),
        ("main", "implementer")
    );
    assert!(
        tmux::find_window(server.name(), &main_session, "implementer")
            .unwrap()
            .is_some()
    );
}
