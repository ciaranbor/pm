use super::*;

#[test]
fn feat_new_with_base_stores_base_in_state() {
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
    feat_new(&FeatNewParams {
        base: Some("login"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "stacked", server.name())
    })
    .unwrap();

    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "stacked").unwrap();
    assert_eq!(state.base, "login");
}

#[test]
fn feat_new_with_base_branches_from_parent() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    // Create parent feature with a commit
    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "parent",
        server.name(),
    ))
    .unwrap();
    let parent_wt = project_path.join("parent");
    std::fs::write(parent_wt.join("parent.txt"), "parent work").unwrap();
    git::stage_file(&parent_wt, "parent.txt").unwrap();
    git::commit(&parent_wt, "parent commit").unwrap();

    // Create stacked feature based on parent
    feat_new(&FeatNewParams {
        base: Some("parent"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "child", server.name())
    })
    .unwrap();

    // Child worktree should have the parent's file
    let child_wt = project_path.join("child");
    assert!(child_wt.join("parent.txt").exists());
}

#[test]
fn feat_new_without_base_defaults_to_main() {
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

    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.base, "main");
}

#[test]
fn resolve_base_returns_explicit_base() {
    let dir = tempdir().unwrap();
    let result = resolve_base(dir.path(), "main", Some("my-branch"), dir.path()).unwrap();
    assert_eq!(result, "my-branch");
}

#[test]
fn resolve_base_detects_branch_from_worktree_cwd() {
    let dir = tempdir().unwrap();
    let project_path = dir.path().join("myproject");
    std::fs::create_dir_all(&project_path).unwrap();
    let main_path = paths::main_worktree(&project_path);
    git::init_repo(&main_path).unwrap();

    git::create_branch(&main_path, "parent").unwrap();
    let parent_wt = project_path.join("parent");
    git::add_worktree(&main_path, &parent_wt, "parent").unwrap();

    // Simulate CWD being inside the parent worktree
    let result = resolve_base(&project_path, "main", None, &parent_wt).unwrap();
    assert_eq!(result, "parent");
}

#[test]
fn resolve_base_outside_project_detects_from_main_worktree() {
    let dir = tempdir().unwrap();
    let project_path = dir.path().join("myproject");
    std::fs::create_dir_all(&project_path).unwrap();
    let main_path = paths::main_worktree(&project_path);
    git::init_repo(&main_path).unwrap();
    git::rename_branch(&main_path, "main", "master").unwrap();

    let outside = dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let result = resolve_base(&project_path, "master", None, &outside).unwrap();
    assert_eq!(result, "master");
}

#[test]
fn resolve_base_falls_back_to_main_branch_on_detached_head() {
    let dir = tempdir().unwrap();
    let project_path = dir.path().join("myproject");
    std::fs::create_dir_all(&project_path).unwrap();
    let main_path = paths::main_worktree(&project_path);
    git::init_repo(&main_path).unwrap();
    git::run_git(&main_path, &["checkout", "--detach"]).unwrap();

    let outside = dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let result = resolve_base(&project_path, "master", None, &outside).unwrap();
    assert_eq!(result, "master");
}

/// A project whose local main is one commit behind its `origin`, which has
/// `ahead.txt`.
fn project_behind_origin(
    dir: &Path,
    server: &TestServer,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let (project_path, projects_dir, _) = server.setup_project(dir);
    let main = paths::main_worktree(&project_path);
    let origin = dir.join("origin.git");
    git::init_bare(&origin).unwrap();
    git::add_remote(&main, "origin", &origin.to_string_lossy()).unwrap();
    std::fs::write(main.join("ahead.txt"), "ahead").unwrap();
    git::stage_file(&main, "ahead.txt").unwrap();
    git::commit(&main, "ahead").unwrap();
    git::push(&main, "origin", "main").unwrap();
    git::run_git(&main, &["reset", "--hard", "HEAD~1"]).unwrap();
    (project_path, projects_dir)
}

#[test]
fn fresh_starts_from_the_fetched_remote_main_without_tracking_it() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir) = project_behind_origin(dir.path(), &server);

    feat_new(&FeatNewParams {
        fresh: Some(true),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();

    assert!(project_path.join("login/ahead.txt").exists());
    let main = paths::main_worktree(&project_path);
    assert_eq!(git::tracking_branch(&main, "login").unwrap(), None);
    let state = FeatureState::load(&paths::features_dir(&project_path), "login").unwrap();
    assert_eq!(state.base, "main");
}

#[test]
fn fresh_base_config_applies_to_main_and_no_fresh_overrides_it() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir) = project_behind_origin(dir.path(), &server);
    let pm_dir = paths::pm_dir(&project_path);
    let mut config = ProjectConfig::load(&pm_dir).unwrap();
    config.project.fresh_base = Some(true);
    config.save(&pm_dir).unwrap();

    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();
    feat_new(&FeatNewParams {
        fresh: Some(false),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "local", server.name())
    })
    .unwrap();

    assert!(project_path.join("login/ahead.txt").exists());
    assert!(!project_path.join("local/ahead.txt").exists());
}

#[test]
fn fresh_base_config_without_a_remote_keeps_the_local_base_where_fresh_fails() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());
    let pm_dir = paths::pm_dir(&project_path);
    let mut config = ProjectConfig::load(&pm_dir).unwrap();
    config.project.fresh_base = Some(true);
    config.save(&pm_dir).unwrap();

    feat_new(&FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();
    let err = feat_new(&FeatNewParams {
        fresh: Some(true),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "fresh", server.name())
    })
    .unwrap_err();

    assert!(err.to_string().contains("--fresh"), "{err}");
    let features_dir = paths::features_dir(&project_path);
    assert!(FeatureState::exists(&features_dir, "login"));
    assert!(!FeatureState::exists(&features_dir, "fresh"));
}
