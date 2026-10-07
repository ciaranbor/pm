use super::*;
use crate::commands::feat_new;

#[test]
fn delete_stacked_feature_merged_into_parent_succeeds() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "parent");

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

    // Merge child into parent so the safety check passes
    let parent_wt = project_path.join("parent");
    git::merge_no_ff(&parent_wt, "child").unwrap();

    // Delete should succeed — child is merged into its base (parent), not main
    feat_delete(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "child",
        false,
        server.name(),
    )
    .unwrap();

    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "child"));
}

#[test]
fn delete_force_stacked_feature_after_parent_gone_removes_worktree_and_branch() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir) = server.setup_orphaned_child(dir.path());

    feat_delete(&project_path, &projects_dir, "child", true, server.name()).unwrap();

    let main = paths::main_worktree(&project_path);
    assert!(!project_path.join("child").exists());
    assert!(!git::branch_exists(&main, "child").unwrap());
    assert!(!FeatureState::exists(
        &paths::features_dir(&project_path),
        "child"
    ));
}

#[test]
fn delete_stacked_feature_after_parent_gone_blocks_without_force() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir) = server.setup_orphaned_child(dir.path());

    let err = feat_delete(&project_path, &projects_dir, "child", false, server.name()).unwrap_err();

    let msg = format!("{err}");
    assert!(msg.contains("base branch 'parent' is gone"), "{msg}");
    assert!(msg.contains("--force child"), "{msg}");
    assert!(msg.contains("git rebase master"), "{msg}");
    assert!(project_path.join("child").exists());
    assert!(FeatureState::exists(
        &paths::features_dir(&project_path),
        "child"
    ));
}

#[test]
fn delete_feature_based_on_a_non_pm_branch_checks_merge_against_it() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_master_project(dir.path());
    let main = paths::main_worktree(&project_path);
    git::create_branch_from(&main, "develop", "master").unwrap();
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
    TestServer::add_feature_commit(&project_path, "child");

    let unmerged = feat_delete(&project_path, &projects_dir, "child", false, server.name());
    assert!(
        matches!(&unmerged, Err(PmError::Unsafe { reason, .. }) if reason.contains("not merged into its base")),
        "{unmerged:?}"
    );

    git::run_git(&main, &["branch", "-f", "develop", "child"]).unwrap();
    feat_delete(&project_path, &projects_dir, "child", false, server.name()).unwrap();

    assert!(!git::branch_exists(&main, "child").unwrap());
    assert!(!FeatureState::exists(
        &paths::features_dir(&project_path),
        "child"
    ));
}

#[test]
fn delete_stacked_feature_not_merged_into_parent_blocks() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "parent");

    // Create stacked feature based on parent with a commit
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

    // Don't merge into parent — should block
    let result = feat_delete(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "child",
        false,
        server.name(),
    );
    assert!(result.is_err());

    let features_dir = paths::features_dir(&project_path);
    assert!(FeatureState::exists(&features_dir, "child"));
}
