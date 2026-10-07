use super::*;

#[test]
fn send_to_active_agent_does_not_spawn() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, session_name, feature) = setup_project_with_tmux(dir.path(), &server);

    // Agent definition must exist so we're genuinely testing the
    // "active agent skips spawn" path, not the "no definition" guard.
    create_agent_definition(&root, "reviewer");

    // Create a fake active agent (window running sleep, not a shell)
    server.spawn_fake_agent(&root, &session_name, &feature, "reviewer");

    let sent = agent_send(
        &root,
        &feature,
        None,
        "reviewer",
        "implementer",
        "hello",
        server.name(),
    )
    .unwrap();
    assert_eq!(
        sent.status,
        "Message 001 sent to 'reviewer' (from 'implementer')"
    );
    assert!(sent.heal.is_none());
}

#[test]
fn send_to_unregistered_agent_errors_and_queues_nothing() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

    // Even with a definition file present, an unregistered (never
    // spawned) agent is not a valid recipient — messaging never
    // conjures a new agent.
    create_agent_definition(&root, "reviewer");

    let result = agent_send(
        &root,
        &feature,
        None,
        "reviewer",
        "implementer",
        "hello",
        server.name(),
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("No active agent called 'reviewer'")
    );

    // Nothing was queued.
    let messages_dir = paths::messages_dir(&root);
    assert!(
        messages::list(&messages_dir, &feature, "reviewer", None)
            .unwrap()
            .is_empty()
    );

    // Agent was not registered.
    let agents_dir = paths::agents_dir(&root);
    let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
    assert!(registry.get("reviewer").is_none());
}

#[test]
fn send_to_stopped_agent_errors() {
    // `pm agent stop` flips active = false. A stopped agent is not a
    // valid recipient — messaging never resurrects a stopped agent.
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

    create_agent_definition(&root, "implementer");

    // Spawn aliased agent, then stop it (active = false).
    crate::commands::agent_spawn::agent_spawn(
        &root,
        &feature,
        "frontend-dev",
        Some("implementer"),
        None,
        server.name(),
    )
    .unwrap();
    crate::commands::agent_stop::agent_stop(&root, &feature, "frontend-dev", server.name())
        .unwrap();

    let result = agent_send(
        &root,
        &feature,
        None,
        "frontend-dev",
        "implementer",
        "hi",
        server.name(),
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("No active agent called 'frontend-dev'")
    );

    // Nothing queued; agent stays inactive.
    let messages_dir = paths::messages_dir(&root);
    assert!(
        messages::list(&messages_dir, &feature, "frontend-dev", None)
            .unwrap()
            .is_empty()
    );
    let agents_dir = paths::agents_dir(&root);
    assert!(
        !AgentRegistry::load(&agents_dir, &feature)
            .unwrap()
            .get("frontend-dev")
            .unwrap()
            .active
    );
}

#[test]
fn send_to_unspawned_agent_errors() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, _session_name, feature) = setup_project_with_tmux(dir.path(), &server);

    // No agent registered at all — send should fail.
    let result = agent_send(
        &root,
        &feature,
        None,
        "reviewer",
        "implementer",
        "hello",
        server.name(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("No active agent called 'reviewer' exists in scope"));
}
