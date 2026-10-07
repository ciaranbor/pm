use super::*;
use crate::commands::init;

#[test]
fn delete_with_uncommitted_changes_is_blocked() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    let worktree = project_path.join("login");
    std::fs::write(worktree.join("test.txt"), "hello").unwrap();
    git::stage_file(&worktree, "test.txt").unwrap();

    let result = feat_delete(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    );
    assert!(result.is_err());

    // State and worktree should persist when safety check blocks
    let features_dir = paths::features_dir(&project_path);
    assert!(FeatureState::exists(&features_dir, "login"));
    assert!(project_path.join("login").exists());
}

#[test]
fn delete_with_force_bypasses_safety_checks() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    let worktree = project_path.join("login");
    std::fs::write(worktree.join("test.txt"), "hello").unwrap();
    git::stage_file(&worktree, "test.txt").unwrap();

    feat_delete(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
        server.name(),
    )
    .unwrap();

    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn delete_merged_branch_succeeds_without_force() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    // Merge the feature branch into main
    let main_repo = paths::main_worktree(&project_path);
    git::merge_no_ff(&main_repo, "login").unwrap();

    feat_delete(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    )
    .unwrap();

    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn delete_nonexistent_feature_fails() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let project_path = dir.path().join(server.scope("myapp"));
    let projects_dir = dir.path().join("registry");
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();

    let result = feat_delete(&project_path, &projects_dir, "nonexistent", false, None);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
}

#[test]
fn delete_with_untracked_files_still_proceeds() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    let worktree = project_path.join("login");
    std::fs::write(worktree.join("untracked.txt"), "hello").unwrap();

    feat_delete(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    )
    .unwrap();

    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn delete_with_unmerged_commits_is_blocked() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    let worktree = project_path.join("login");
    std::fs::write(worktree.join("feature.txt"), "content").unwrap();
    git::stage_file(&worktree, "feature.txt").unwrap();
    git::commit(&worktree, "feature work").unwrap();

    let result = feat_delete(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    );
    assert!(result.is_err());

    let features_dir = paths::features_dir(&project_path);
    assert!(FeatureState::exists(&features_dir, "login"));
}
