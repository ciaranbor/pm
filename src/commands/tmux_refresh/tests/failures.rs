use super::*;

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
        let (project, projects_dir, project_name) =
            server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
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
    let (project, projects_dir, project_name) =
        server.setup_project_with_feature(dir.path(), "login");
    let session = tmux::session_name(&project_name, "login");
    refresh(&projects_dir, server.name()).unwrap();
    feat_status(
        &project,
        "login",
        Progress::Blocked,
        Some("which DB?"),
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();

    let mut before = published(&server);
    before.clients.push(Client::named("client-gone"));
    let snapshot = attention::all(&projects_dir, server.name()).unwrap();
    write(server.name(), &commands(&snapshot, &before, Utc::now())).unwrap();

    let now = published(&server);
    assert_eq!(values(&now.sessions, &session, &[ATTENTION]), ["blocked"]);
}
