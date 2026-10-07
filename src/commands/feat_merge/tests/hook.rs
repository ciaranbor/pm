//! The post-merge hook run in the base session's hook window.

use super::*;

#[test]
fn merge_runs_default_post_merge_hook_in_main_session() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let project_name = server.scope("myapp");
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    TestServer::add_feature_commit(&project_path, "login");

    // Main session should have 1 window before merge
    let before =
        tmux::list_windows(server.name(), &tmux::session_name(&project_name, "main")).unwrap();
    assert_eq!(before, 1);

    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
        server.name(),
    )
    .unwrap();

    // Main session should now have 2 windows: original + hook window
    let after =
        tmux::list_windows(server.name(), &tmux::session_name(&project_name, "main")).unwrap();
    assert_eq!(after, 2);
    // Hook window should be named "hook"
    let target = tmux::find_window(
        server.name(),
        &tmux::session_name(&project_name, "main"),
        "hook",
    )
    .unwrap();
    assert!(target.is_some());
}

#[test]
fn merge_reuses_existing_hook_window() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let project_name = server.scope("myapp");
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    TestServer::add_feature_commit(&project_path, "login");
    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
        server.name(),
    )
    .unwrap();

    // Create a second feature and merge it too
    feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "api",
        server.name(),
    ))
    .unwrap();
    let worktree = project_path.join("api");
    std::fs::write(worktree.join("api.txt"), "api work").unwrap();
    git::stage_file(&worktree, "api.txt").unwrap();
    git::commit(&worktree, "api work").unwrap();
    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "api",
        true,
        server.name(),
    )
    .unwrap();

    // Should still have just 2 windows — the hook window was reused, not duplicated
    let windows =
        tmux::list_windows(server.name(), &tmux::session_name(&project_name, "main")).unwrap();
    assert_eq!(windows, 2);
}

#[test]
fn merge_skips_hook_when_script_removed() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let project_name = server.scope("myapp");
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    // Remove the bootstrapped hook script
    std::fs::remove_file(project_path.join(hooks::POST_MERGE_PATH)).unwrap();

    TestServer::add_feature_commit(&project_path, "login");
    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
        server.name(),
    )
    .unwrap();

    // Main session should still have just 1 window
    let windows =
        tmux::list_windows(server.name(), &tmux::session_name(&project_name, "main")).unwrap();
    assert_eq!(windows, 1);
}

#[test]
fn merge_hook_succeeds_when_main_session_absent() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let project_name = server.scope("myapp");
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    TestServer::add_feature_commit(&project_path, "login");

    // Kill the main session before merging
    tmux::kill_session(server.name(), &tmux::session_name(&project_name, "main")).unwrap();

    // Merge should still succeed — hook skip is non-fatal
    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
        server.name(),
    )
    .unwrap();

    // Verify the merge itself worked
    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.status, FeatureStatus::Merged);
}
