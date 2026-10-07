use super::*;

#[test]
fn open_succeeds_with_orphaned_feature_state() {
    // open should warn about drift but still complete — orphaned features
    // don't block restoring the rest of the project.
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

    // Orphan the feature, then kill the main session
    let main_repo = paths::main_worktree(&project_path);
    git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
    git::delete_branch(&main_repo, "login").unwrap();
    tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();
    tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();

    // open should still succeed; the orphaned feature is just warned about.
    let result = open(&project_path, &projects_dir, server.name()).unwrap();
    assert!(
        tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap(),
        "main session should be restored despite drift warning"
    );
    // login session NOT recreated (no worktree present)
    assert!(!tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
    assert_eq!(result.sessions_restored, 1);
}
#[test]
fn open_creates_main_session_when_missing() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();

    // Kill the main session that init created
    tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();
    assert!(!tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());

    open(&project_path, &projects_dir, server.name()).unwrap();

    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());
}

#[test]
fn open_skips_existing_main_session() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();

    // Main session already exists from init — open should not fail
    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());

    open(&project_path, &projects_dir, server.name()).unwrap();

    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());
}

#[test]
fn open_recreates_missing_feature_sessions() {
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

    // Kill the feature session
    tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();
    assert!(!tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());

    open(&project_path, &projects_dir, server.name()).unwrap();

    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
}

#[test]
fn open_skips_existing_feature_sessions() {
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

    // Feature session exists — open should not fail
    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());

    open(&project_path, &projects_dir, server.name()).unwrap();

    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
}

#[test]
fn open_skips_merged_features() {
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

    // Manually set feature status to merged
    let features_dir = paths::features_dir(&project_path);
    let mut state = FeatureState::load(&features_dir, "login").unwrap();
    state.status = crate::state::feature::FeatureStatus::Merged;
    state.save(&features_dir, "login").unwrap();

    // Kill the feature session
    tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();

    open(&project_path, &projects_dir, server.name()).unwrap();

    // Should NOT recreate session for merged feature
    assert!(!tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
}

#[test]
fn open_with_no_features_only_creates_main() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();

    // Kill main
    tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();

    open(&project_path, &projects_dir, server.name()).unwrap();

    let sessions: Vec<_> = tmux::list_sessions(server.name())
        .unwrap()
        .into_iter()
        .filter(|s| s.starts_with(&format!("{name}/")))
        .collect();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0], tmux::session_name(&name, "main"));
}

#[test]
fn open_errors_when_main_worktree_missing() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();

    // Kill session and delete the main worktree
    tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();
    std::fs::remove_dir_all(paths::main_worktree(&project_path)).unwrap();

    let result = open(&project_path, &projects_dir, server.name());
    assert!(result.is_err());
}

#[test]
fn open_skips_feature_with_missing_worktree() {
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
    feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "api",
        server.name(),
    ))
    .unwrap();

    // Kill sessions and delete only login's worktree
    tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();
    tmux::kill_session(server.name(), &tmux::session_name(&name, "api")).unwrap();
    std::fs::remove_dir_all(project_path.join("login")).unwrap();

    open(&project_path, &projects_dir, server.name()).unwrap();

    // login skipped (missing worktree), api recreated
    assert!(!tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "api")).unwrap());
}

#[test]
fn open_returns_zero_counts_when_nothing_restored() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();

    // All sessions already exist from init
    let result = open(&project_path, &projects_dir, server.name()).unwrap();
    assert_eq!(result.sessions_restored, 0);
    assert_eq!(result.agents_respawned, 0);
}

#[test]
fn open_returns_session_count_when_restored() {
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

    // Kill both sessions
    tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();
    tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();

    let result = open(&project_path, &projects_dir, server.name()).unwrap();
    assert_eq!(result.sessions_restored, 2); // main + login
    assert_eq!(result.agents_respawned, 0);
}
