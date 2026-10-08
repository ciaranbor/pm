//! `feat_new` end to end: a feature's resources, its name, and the limits
//! and rollbacks around creating it.

mod base;
mod context;
mod team;

use super::*;
use crate::hooks;
use crate::testing::TestServer;
use tempfile::tempdir;

#[test]
fn feat_new_creates_all_resources() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());
    let before = Utc::now();

    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();

    // State file with correct status and fields
    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.status, FeatureStatus::Wip);
    assert_eq!(state.branch, "login");
    assert_eq!(state.worktree, "login");
    assert!(state.created >= before);
    assert!(state.last_active >= state.created);

    // Git branch exists
    let main_path = paths::main_worktree(&project_path);
    assert!(git::branch_exists(&main_path, "login").unwrap());

    // Worktree directory exists
    let worktree_path = project_path.join("login");
    assert!(worktree_path.exists());
    assert!(worktree_path.is_dir());

    // Tmux session exists
    assert!(tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap());
}

#[test]
fn feat_new_refuses_a_name_with_an_untriaged_summary() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project_no_tmux(dir.path());
    let summary = paths::summary_path(&project_path, "login");
    std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
    std::fs::write(&summary, "earlier notes").unwrap();

    let err = feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap_err()
    .to_string();

    assert!(err.contains("earlier feature 'login'"), "{err}");
    assert!(err.contains(&summary.display().to_string()), "{err}");
    let main_wt = paths::main_worktree(&project_path);
    assert!(!crate::git::branch_exists(&main_wt, "login").unwrap());
    assert!(!project_path.join("login").exists());
    assert!(!FeatureState::exists(
        &paths::features_dir(&project_path),
        "login"
    ));
    assert_eq!(std::fs::read_to_string(&summary).unwrap(), "earlier notes");
}

#[test]
fn feat_new_duplicate_name_fails() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();
    let result = feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ));

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        PmError::FeatureAlreadyExists(_)
    ));
}

#[test]
fn feat_new_tmux_failure_cleans_up_all_resources() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

    // Pre-create a tmux session with the name feat_new will use,
    // so create_session fails with "duplicate session"
    tmux::create_session(
        server.name(),
        &tmux::session_name(&project_name, "login"),
        dir.path(),
    )
    .unwrap();

    let result = feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ));
    assert!(result.is_err());

    // State file should be cleaned up
    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));

    // Branch and worktree should be cleaned up
    let main_path = paths::main_worktree(&project_path);
    assert!(!git::branch_exists(&main_path, "login").unwrap());
    assert!(!project_path.join("login").exists());
}

#[test]
fn feat_new_worktree_failure_cleans_up_branch_and_state() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    // Pre-create the worktree path so add_worktree fails
    std::fs::create_dir(project_path.join("login")).unwrap();
    std::fs::write(project_path.join("login").join("blocker.txt"), "").unwrap();

    let result = feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ));
    assert!(result.is_err());

    // State file should be cleaned up
    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));

    // Branch should be cleaned up (worktree was never created by git)
    let main_path = paths::main_worktree(&project_path);
    assert!(!git::branch_exists(&main_path, "login").unwrap());
    assert!(
        project_path.join("login/blocker.txt").exists(),
        "the directory in the way is the user's"
    );
}

#[test]
fn feat_new_without_context_has_shell_and_hook_windows() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();

    // 2 windows: default shell + hook window
    let session = tmux::session_name(&project_name, "login");
    let windows = tmux::list_windows(server.name(), &session).unwrap();
    assert_eq!(windows, 2);
    // Hook window should be named "hook"
    let target = tmux::find_window(server.name(), &session, "hook").unwrap();
    assert!(target.is_some());

    // No context → no TASK.md (ever) and no queued messages.
    let task_md = project_path.join("login").join("TASK.md");
    assert!(!task_md.exists());
    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.context, "");
    // No workflow either when --workflow not given.
    assert!(state.workflow.is_none());
}

#[test]
fn feat_new_hook_receives_pm_env() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

    let hook_path = project_path.join(hooks::POST_CREATE_PATH);
    std::fs::write(
        &hook_path,
        "#!/bin/sh\nprintf '%s\\n%s\\n%s\\n%s\\n%s\\n' \
         \"$PM_PROJECT_ROOT\" \"$PM_MAIN_WORKTREE\" \"$PM_WORKTREE\" \"$PM_SESSION\" \
         \"$PM_FEATURE\" > \"$PM_WORKTREE/hook-env.tmp\" && \
         mv \"$PM_WORKTREE/hook-env.tmp\" \"$PM_WORKTREE/hook-env.txt\"\n",
    )
    .unwrap();

    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();

    let out = project_path.join("login/hook-env.txt");
    let mut content = None;
    for _ in 0..500 {
        if let Ok(c) = std::fs::read_to_string(&out) {
            content = Some(c);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let content = content.unwrap_or_else(|| {
        let hook = format!("{}:hook", tmux::session_name(&project_name, "login"));
        panic!(
            "hook never wrote its environment; running {:?}, pane:\n{:?}",
            tmux::pane_command(server.name(), &hook),
            tmux::capture_pane(server.name(), &hook),
        )
    });
    let expected = format!(
        "{}\n{}\n{}\n{}\nlogin\n",
        project_path.display(),
        project_path.join("main").display(),
        project_path.join("login").display(),
        tmux::session_name(&project_name, "login"),
    );
    assert_eq!(content, expected);
}

#[test]
fn feat_new_skips_hook_when_script_removed() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

    // Remove the bootstrapped hook script
    std::fs::remove_file(project_path.join(hooks::POST_CREATE_PATH)).unwrap();

    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();

    // Only 1 window — hook was skipped because file is missing
    let windows =
        tmux::list_windows(server.name(), &tmux::session_name(&project_name, "login")).unwrap();
    assert_eq!(windows, 1);
}

#[test]
fn sanitize_replaces_slashes_with_dashes() {
    assert_eq!(
        sanitize_feature_name("ciaran/eval", None).unwrap(),
        "ciaran-eval"
    );
    assert_eq!(
        sanitize_feature_name("feat/deep/nested", None).unwrap(),
        "feat-deep-nested"
    );
    assert_eq!(sanitize_feature_name("simple", None).unwrap(), "simple");
}

#[test]
fn sanitize_uses_override_when_provided() {
    assert_eq!(
        sanitize_feature_name("ciaran/eval", Some("eval")).unwrap(),
        "eval"
    );
}

#[test]
fn sanitize_refuses_the_main_scopes_name() {
    for (branch, name) in [("feat/x", Some("main")), ("main", None)] {
        assert!(matches!(
            sanitize_feature_name(branch, name).unwrap_err(),
            PmError::ReservedFeatureName(n) if n == "main"
        ));
    }
}

#[test]
fn sanitize_rejects_override_with_slash() {
    let result = sanitize_feature_name("ciaran/eval", Some("foo/bar"));
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        PmError::InvalidFeatureName(_)
    ));
}

#[test]
fn feat_new_slash_collision_detected() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    // Create "ciaran-login" first
    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "ciaran-login",
        server.name(),
    ))
    .unwrap();

    // "ciaran/login" sanitizes to "ciaran-login" — should conflict
    let result = feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "ciaran/login",
        server.name(),
    ));
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        PmError::FeatureAlreadyExists(_)
    ));
}

#[test]
fn feat_new_slash_branch_sanitizes_feature_name() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "ciaran/login",
        server.name(),
    ))
    .unwrap();

    // Feature name should be sanitized
    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "ciaran-login").unwrap();
    assert_eq!(state.status, FeatureStatus::Wip);
    assert_eq!(state.branch, "ciaran/login");
    assert_eq!(state.worktree, "ciaran-login");

    // Worktree dir uses sanitized name
    assert!(project_path.join("ciaran-login").exists());

    // Tmux session uses sanitized name
    assert!(
        tmux::has_session(
            server.name(),
            &tmux::session_name(&project_name, "ciaran-login")
        )
        .unwrap()
    );
}

#[test]
fn feat_new_with_name_override() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

    feat_new(&FeatNewParams {
        name_override: Some("eval"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "ciaran/eval", server.name())
    })
    .unwrap();

    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "eval").unwrap();
    assert_eq!(state.branch, "ciaran/eval");
    assert_eq!(state.worktree, "eval");
    assert!(project_path.join("eval").exists());
    assert!(tmux::has_session(server.name(), &tmux::session_name(&project_name, "eval")).unwrap());
}

#[test]
fn feat_new_blocked_by_feature_limit() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    // Set max_features = 1
    let pm_dir = paths::pm_dir(&project_path);
    let mut config = ProjectConfig::load(&pm_dir).unwrap();
    config.project.max_features = Some(1);
    config.save(&pm_dir).unwrap();

    // Create first feature — should succeed
    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "first",
        server.name(),
    ))
    .unwrap();

    // Second feature should be blocked
    let result = feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "second",
        server.name(),
    ));
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, PmError::SafetyCheck(_)));
    assert!(err.to_string().contains("Feature limit reached"));
}

#[test]
fn feat_new_allowed_under_feature_limit() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    // Set max_features = 2
    let pm_dir = paths::pm_dir(&project_path);
    let mut config = ProjectConfig::load(&pm_dir).unwrap();
    config.project.max_features = Some(2);
    config.save(&pm_dir).unwrap();

    // First feature
    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "first",
        server.name(),
    ))
    .unwrap();

    // Second feature should also succeed (2/2 would block, but 1/2 is fine)
    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "second",
        server.name(),
    ))
    .unwrap();

    // Verify both exist
    let features_dir = paths::features_dir(&project_path);
    assert!(FeatureState::exists(&features_dir, "first"));
    assert!(FeatureState::exists(&features_dir, "second"));
}
