//! Which base a feature merges into: legacy defaults, stacked features, and
//! bases pm does not manage.

use super::*;

#[test]
fn merge_legacy_feature_on_master_default_project_lands_on_master() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_master_project(dir.path());
    let main_repo = paths::main_worktree(&project_path);

    feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
        &project_path,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();
    // A feature recorded before `base` existed.
    let features_dir = paths::features_dir(&project_path);
    let mut state = FeatureState::load(&features_dir, "login").unwrap();
    state.base = String::new();
    state.save(&features_dir, "login").unwrap();
    TestServer::add_feature_commit(&project_path, "login");

    feat_merge(&project_path, &projects_dir, "login", false, server.name()).unwrap();

    assert_eq!(git::current_branch(&main_repo).unwrap(), "master");
    assert!(main_repo.join("feature.txt").exists());
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn merge_stacked_feature_after_parent_gone_refuses_with_hint() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir) = server.setup_orphaned_child(dir.path());

    let err = feat_merge(&project_path, &projects_dir, "child", false, server.name()).unwrap_err();

    let msg = format!("{err}");
    assert!(msg.contains("cannot merge feature 'child'"), "{msg}");
    assert!(msg.contains("base branch 'parent' is gone"), "{msg}");
    assert!(msg.contains("git rebase master"), "{msg}");
    let main_repo = paths::main_worktree(&project_path);
    assert!(!main_repo.join("child.txt").exists());
    assert!(FeatureState::exists(
        &paths::features_dir(&project_path),
        "child"
    ));
}

#[test]
fn merge_feature_based_on_a_non_pm_branch_refuses_with_adopt_hint() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_master_project(dir.path());
    let main_repo = paths::main_worktree(&project_path);
    git::create_branch_from(&main_repo, "develop", "master").unwrap();
    feat_new::feat_new(&feat_new::FeatNewParams {
        project_root: &project_path,
        projects_dir: &projects_dir,
        name: "child",
        name_override: None,
        context: None,
        base: Some("develop"),
        workflow: None,
        tmux_server: server.name(),
    })
    .unwrap();

    let err = feat_merge(&project_path, &projects_dir, "child", false, server.name()).unwrap_err();

    let msg = format!("{err}");
    assert!(msg.contains("'develop' is not checked out"), "{msg}");
    assert!(msg.contains("pm feat adopt develop"), "{msg}");
    assert!(!msg.contains("gone"), "{msg}");
}

#[test]
fn merge_stacked_feature_merges_into_parent_worktree() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "parent");

    // Add a commit to parent
    let parent_wt = project_path.join("parent");
    std::fs::write(parent_wt.join("parent.txt"), "parent work").unwrap();
    git::stage_file(&parent_wt, "parent.txt").unwrap();
    git::commit(&parent_wt, "parent commit").unwrap();

    // Create stacked feature based on parent
    feat_new::feat_new(&feat_new::FeatNewParams {
        project_root: &project_path,
        projects_dir: &TestServer::registry_dir(&project_path),
        name: "child",
        name_override: None,
        context: None,
        base: Some("parent"),
        workflow: None,
        tmux_server: server.name(),
    })
    .unwrap();
    let child_wt = project_path.join("child");
    std::fs::write(child_wt.join("child.txt"), "child work").unwrap();
    git::stage_file(&child_wt, "child.txt").unwrap();
    git::commit(&child_wt, "child commit").unwrap();

    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "child",
        true,
        server.name(),
    )
    .unwrap();

    // Child's changes should appear in parent worktree, not main
    assert!(parent_wt.join("child.txt").exists());
    let main_repo = paths::main_worktree(&project_path);
    assert!(!main_repo.join("child.txt").exists());
}

#[test]
fn merge_stacked_feature_cleans_up_by_default() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "parent");

    feat_new::feat_new(&feat_new::FeatNewParams {
        project_root: &project_path,
        projects_dir: &TestServer::registry_dir(&project_path),
        name: "child",
        name_override: None,
        context: None,
        base: Some("parent"),
        workflow: None,
        tmux_server: server.name(),
    })
    .unwrap();
    let child_wt = project_path.join("child");
    std::fs::write(child_wt.join("child.txt"), "child work").unwrap();
    git::stage_file(&child_wt, "child.txt").unwrap();
    git::commit(&child_wt, "child commit").unwrap();

    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "child",
        false,
        server.name(),
    )
    .unwrap();

    // Cleaned up
    assert!(!project_path.join("child").exists());
    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "child"));
}
