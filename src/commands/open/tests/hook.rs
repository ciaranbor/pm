use super::*;

#[test]
fn open_backfills_hook_scripts_for_existing_projects() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();

    // Simulate a pre-hooks project by removing the bootstrapped hooks
    std::fs::remove_dir_all(project_path.join(".pm/hooks")).unwrap();
    assert!(!project_path.join(hooks::POST_CREATE_PATH).exists());

    open(&project_path, &projects_dir, server.name()).unwrap();

    assert!(project_path.join(hooks::POST_CREATE_PATH).is_file());
    assert!(project_path.join(hooks::POST_MERGE_PATH).is_file());
    assert!(project_path.join(hooks::RESTORE_PATH).is_file());
}

#[test]
fn open_runs_restore_hook_for_new_sessions() {
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

    // Create a restore hook
    let restore_path = project_path.join(hooks::RESTORE_PATH);
    std::fs::write(&restore_path, "#!/bin/sh\necho restored\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&restore_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Kill all sessions to force recreation
    tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();
    tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();

    open(&project_path, &projects_dir, server.name()).unwrap();

    // Verify sessions were created and hook windows exist (restore hook ran)
    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());
    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
    assert!(
        tmux::find_window(server.name(), &tmux::session_name(&name, "main"), "hook")
            .unwrap()
            .is_some()
    );
    assert!(
        tmux::find_window(server.name(), &tmux::session_name(&name, "login"), "hook")
            .unwrap()
            .is_some()
    );
}

#[test]
fn open_skips_restore_hook_for_existing_sessions() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let name = server.scope("myapp");
    let project_path = dir.path().join(&name);
    let projects_dir = TestServer::registry_dir(&project_path);
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();

    // Create a restore hook
    let restore_path = project_path.join(hooks::RESTORE_PATH);
    std::fs::write(&restore_path, "#!/bin/sh\necho restored\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&restore_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Sessions already exist from init — open should NOT run restore hook
    open(&project_path, &projects_dir, server.name()).unwrap();

    // No hook window should exist since sessions were not recreated
    assert!(
        tmux::find_window(server.name(), &tmux::session_name(&name, "main"), "hook")
            .unwrap()
            .is_none()
    );
}
