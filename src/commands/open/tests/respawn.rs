use super::*;

#[test]
fn open_respawns_agents_for_restored_features() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();
    feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();

    // Register an agent for the feature
    let agents_dir = paths::agents_dir(&project_path);
    let mut registry = AgentRegistry::default();
    registry.register(
        "reviewer",
        crate::state::agent::AgentEntry {
            agent_type: AgentType::Agent,
            session_id: String::new(),
            window_name: "reviewer".to_string(),
            active: true,
            agent_definition: None,
            harness: crate::harness::Harness::ClaudeCode,
            spawned_at: None,
        },
    );
    registry.save(&agents_dir, "login").unwrap();

    // Kill the feature session (simulating reboot)
    tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();

    let result = open(&project_path, &projects_dir, server.name()).unwrap();

    // Session restored and agent respawned
    assert_eq!(result.sessions_restored, 1);
    assert_eq!(result.agents_respawned, 1);

    // Agent window should exist in the restored session
    assert!(
        tmux::find_window(
            server.name(),
            &tmux::session_name(&name, "login"),
            "reviewer"
        )
        .unwrap()
        .is_some()
    );
}

#[test]
fn open_clears_stale_active_flags() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();
    feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();

    // Register an agent marked active (but its window won't exist after session kill)
    let agents_dir = paths::agents_dir(&project_path);
    let mut registry = AgentRegistry::default();
    registry.register(
        "reviewer",
        crate::state::agent::AgentEntry {
            agent_type: AgentType::Agent,
            session_id: String::new(),
            window_name: "reviewer".to_string(),
            active: true,
            agent_definition: None,
            harness: crate::harness::Harness::ClaudeCode,
            spawned_at: None,
        },
    );
    registry.save(&agents_dir, "login").unwrap();

    // Kill the feature session
    tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();

    open(&project_path, &projects_dir, server.name()).unwrap();

    // After open, the agent should be respawned (window exists)
    let session_name = tmux::session_name(&name, "login");
    let window = tmux::find_window(server.name(), &session_name, "reviewer").unwrap();
    assert!(window.is_some(), "reviewer window should be respawned");
}

#[test]
fn open_respawns_agents_for_main_scope() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();

    // Install its definition so pre-spawn validation resolves, then
    // register an agent in the main scope.
    let orch_def = paths::main_worktree(&project_path).join(".agents/agents/orchestrator.md");
    std::fs::create_dir_all(orch_def.parent().unwrap()).unwrap();
    std::fs::write(&orch_def, "# stub").unwrap();
    let agents_dir = paths::agents_dir(&project_path);
    let mut registry = AgentRegistry::default();
    registry.register(
        "orchestrator",
        crate::state::agent::AgentEntry {
            agent_type: AgentType::Agent,
            session_id: String::new(),
            window_name: "orchestrator".to_string(),
            active: true,
            agent_definition: None,
            harness: crate::harness::Harness::ClaudeCode,
            spawned_at: None,
        },
    );
    registry.save(&agents_dir, "main").unwrap();

    // Kill the main session (simulating reboot)
    tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();

    let result = open(&project_path, &projects_dir, server.name()).unwrap();

    // Session restored and agent respawned
    assert_eq!(result.sessions_restored, 1);
    assert_eq!(result.agents_respawned, 1);

    // Agent window should exist in the restored main session
    assert!(
        tmux::find_window(
            server.name(),
            &tmux::session_name(&name, "main"),
            "orchestrator"
        )
        .unwrap()
        .is_some()
    );
}

#[test]
fn open_respawns_agents_when_session_already_exists() {
    // This is the core bug fix: when tmux-resurrect preserves the session
    // but the agent window is gone, open should still respawn agents.
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();
    feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();

    // Register an agent marked active (simulating a previously running agent)
    let agents_dir = paths::agents_dir(&project_path);
    let mut registry = AgentRegistry::default();
    registry.register(
        "reviewer",
        crate::state::agent::AgentEntry {
            agent_type: AgentType::Agent,
            session_id: String::new(),
            window_name: "reviewer".to_string(),
            active: true,
            agent_definition: None,
            harness: crate::harness::Harness::ClaudeCode,
            spawned_at: None,
        },
    );
    registry.save(&agents_dir, "login").unwrap();

    // Session still exists (NOT killed) — simulates tmux-resurrect preserving it.
    // But the agent window doesn't exist (it was in a different window that wasn't preserved).
    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());

    let result = open(&project_path, &projects_dir, server.name()).unwrap();

    // Session was NOT restored (it already existed)
    assert_eq!(result.sessions_restored, 0);
    // But agent should still be respawned (exactly one registered)
    assert_eq!(result.agents_respawned, 1);

    // Agent window should exist
    assert!(
        tmux::find_window(
            server.name(),
            &tmux::session_name(&name, "login"),
            "reviewer"
        )
        .unwrap()
        .is_some()
    );
}

#[test]
fn open_idempotent_reports_zero_agents_respawned() {
    // Regression: a second `pm open` in a row was reporting
    // "Respawned N agents" even though every agent was already alive.
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();
    feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();

    // Spawn the agent fresh so its window actually exists.
    let agents_dir = paths::agents_dir(&project_path);
    let mut registry = AgentRegistry::default();
    registry.register(
        "reviewer",
        crate::state::agent::AgentEntry {
            agent_type: AgentType::Agent,
            session_id: String::new(),
            window_name: "reviewer".to_string(),
            active: true,
            agent_definition: None,
            harness: crate::harness::Harness::ClaudeCode,
            spawned_at: None,
        },
    );
    registry.save(&agents_dir, "login").unwrap();

    // First open: agent window doesn't exist yet → spawn count = 1
    let first = open(&project_path, &projects_dir, server.name()).unwrap();
    assert_eq!(first.agents_respawned, 1);

    // Second open: nothing to do, every agent is already running.
    let second = open(&project_path, &projects_dir, server.name()).unwrap();
    assert_eq!(
        second.sessions_restored, 0,
        "no sessions should be created on idempotent re-run"
    );
    assert_eq!(
        second.agents_respawned, 0,
        "no agents should be respawned on idempotent re-run"
    );
}
