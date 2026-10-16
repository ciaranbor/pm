use super::*;

#[test]
fn main_agents_get_badges_and_main_carries_its_project_and_its_agents_badge() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, project_name) =
        server.setup_project_with_feature(dir.path(), "login");
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
            "",
        ]
    );
    assert_eq!(
        values(&now.sessions, &main, SESSION_OPTIONS),
        [
            project_name.as_str(),
            "",
            "wip",
            "",
            "",
            "#[fg=colour245]\u{f252} #[fg=yellow]\u{f0e0}#[default]",
            "#[fg=colour245]\u{f252} idle#[default] #[fg=yellow]\u{f0e0} 1#[default]",
            "",
            "",
            ""
        ],
        "main's own badge while not blocked"
    );

    let agents_dir = paths::agents_dir(&project);
    let mut registry = AgentRegistry::load(&agents_dir, "main").unwrap();
    registry.agents.remove("main");
    registry.save(&agents_dir, "main").unwrap();
    refresh(&projects_dir, server.name()).unwrap();

    let now = published(&server);
    assert_eq!(
        values(&now.windows, &orchestrator, WINDOW_OPTIONS),
        ["", "", "", "", "", ""]
    );
}

#[test]
fn an_agents_latest_line_survives_its_own_window_write() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, project_name) =
        server.setup_project_with_feature(dir.path(), "login");
    let session = tmux::session_name(&project_name, "login");
    let window = server.spawn_idle_fake_agent(&project, &session, "login", "implementer");
    let transcript = dir.path().join("s.jsonl");
    let reply = serde_json::json!({"type": "assistant", "uuid": "a1",
        "message": {"content": [{"type": "text", "text": "## Fixed\nthe #[fg=red] bug"}]}});
    std::fs::write(&transcript, format!("{reply}\n")).unwrap();
    crate::state::runtime::write_session_path(
        &project,
        "login",
        "implementer",
        crate::state::runtime::SessionPath::Transcript,
        Some(&transcript),
    )
    .unwrap();

    refresh(&projects_dir, server.name()).unwrap();
    let latest = |server: &TestServer| {
        values(
            &published(server).windows,
            &window,
            &[AGENT_STATE, AGENT_LATEST],
        )
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>()
    };
    assert_eq!(latest(&server), ["idle", "Fixed the ##[fg=red] bug"]);

    let pane = std::process::Command::new("tmux")
        .args(["-L", server.name().unwrap(), "display", "-p", "-t", &window])
        .arg("#{pane_id}")
        .output()
        .unwrap();
    let pane = String::from_utf8_lossy(&pane.stdout).trim().to_string();
    let mut own = crate::commands::tmux_push::AgentWindow::new(server.name(), &pane, "implementer");
    own.publish(AgentState::Busy, 0);
    let start = std::time::Instant::now();
    while latest(&server)[0] != "busy" {
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(latest(&server), ["busy", "Fixed the ##[fg=red] bug"]);
}
