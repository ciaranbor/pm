use super::*;

#[test]
fn open_all_reopens_every_project_and_skips_a_missing_root() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let projects_dir = dir.path().join("registry");
    let alpha = server.scope("alpha");
    let alpha_path = dir.path().join(&alpha);
    init::init(&alpha_path, &projects_dir, None, server.name()).unwrap();
    feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
        &alpha_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();
    let beta = server.scope("beta");
    init::init(&dir.path().join(&beta), &projects_dir, None, server.name()).unwrap();
    ProjectEntry {
        root: dir.path().join("gone").to_string_lossy().to_string(),
        main_branch: "main".to_string(),
        repo_url: None,
        state_remote: None,
    }
    .save(&projects_dir, "ghost")
    .unwrap();
    let sessions = [
        tmux::session_name(&alpha, "main"),
        tmux::session_name(&alpha, "login"),
        tmux::session_name(&beta, "main"),
    ];
    for s in &sessions {
        tmux::kill_session(server.name(), s).unwrap();
    }

    let outcomes: Vec<(String, String)> = open_all(&projects_dir, server.name(), |_, _| {})
        .unwrap()
        .into_iter()
        .map(|(name, outcome)| {
            let label = match outcome {
                ProjectOpen::Opened(r) => format!("restored {}", r.sessions_restored),
                ProjectOpen::RootMissing(_) => "root missing".to_string(),
                ProjectOpen::Failed(_) => "failed".to_string(),
            };
            (name, label)
        })
        .collect();

    for s in &sessions {
        assert!(
            tmux::has_session(server.name(), s).unwrap(),
            "{s} not reopened"
        );
    }
    assert_eq!(
        outcomes,
        [
            ("ghost".to_string(), "root missing".to_string()),
            (alpha, "restored 2".to_string()),
            (beta, "restored 1".to_string()),
        ]
    );
}

#[test]
fn open_all_continues_past_a_project_that_fails() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let projects_dir = dir.path().join("registry");
    let broken = server.scope("broken");
    let broken_path = dir.path().join(&broken);
    init::init(&broken_path, &projects_dir, None, server.name()).unwrap();
    tmux::kill_session(server.name(), &tmux::session_name(&broken, "main")).unwrap();
    std::fs::remove_dir_all(paths::main_worktree(&broken_path)).unwrap();
    let healthy = server.scope("healthy");
    init::init(
        &dir.path().join(&healthy),
        &projects_dir,
        None,
        server.name(),
    )
    .unwrap();
    tmux::kill_session(server.name(), &tmux::session_name(&healthy, "main")).unwrap();

    let outcomes: Vec<(String, String)> = open_all(&projects_dir, server.name(), |_, _| {})
        .unwrap()
        .into_iter()
        .map(|(name, outcome)| {
            let label = match outcome {
                ProjectOpen::Opened(r) => format!("restored {}", r.sessions_restored),
                ProjectOpen::RootMissing(_) => "root missing".to_string(),
                ProjectOpen::Failed(_) => "failed".to_string(),
            };
            (name, label)
        })
        .collect();

    assert_eq!(
        outcomes,
        [
            (broken, "failed".to_string()),
            (healthy.clone(), "restored 1".to_string()),
        ]
    );
    assert!(tmux::has_session(server.name(), &tmux::session_name(&healthy, "main")).unwrap());
}
