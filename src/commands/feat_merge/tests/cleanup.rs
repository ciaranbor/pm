//! Ending the feature after a merge, `--keep`, and retries of a merge
//! already recorded.

use super::*;

#[test]
fn merge_cleans_up_by_default() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let project_name = server.scope("myapp");
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    TestServer::add_feature_commit(&project_path, "login");

    // Verify session exists before merge
    assert!(tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap());

    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    )
    .unwrap();

    // Session killed
    assert!(
        !tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
    );
    // Worktree removed
    assert!(!project_path.join("login").exists());
    // Branch deleted
    let main_repo = paths::main_worktree(&project_path);
    assert!(!git::branch_exists(&main_repo, "login").unwrap());
    // State removed
    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn merge_with_keep_leaves_feature_intact() {
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

    // Session still exists
    assert!(tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap());
    // Worktree still exists
    assert!(project_path.join("login").exists());
    // Branch still exists
    let main_repo = paths::main_worktree(&project_path);
    assert!(git::branch_exists(&main_repo, "login").unwrap());
    // State still exists, but status is Merged
    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.status, FeatureStatus::Merged);
}

#[test]
fn merge_already_merged_feature_with_keep_is_noop() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
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

    // Second merge with --keep should succeed (no-op)
    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
        server.name(),
    )
    .unwrap();

    // State should still be Merged
    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.status, FeatureStatus::Merged);
}

#[test]
fn merge_already_merged_feature_cleans_up() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let project_name = server.scope("myapp");
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    TestServer::add_feature_commit(&project_path, "login");

    // First merge with --keep to set status to Merged
    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
        server.name(),
    )
    .unwrap();

    // Second merge without --keep should clean up
    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    )
    .unwrap();

    // Session killed
    assert!(
        !tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
    );
    // Worktree removed
    assert!(!project_path.join("login").exists());
    // Branch deleted
    let main_repo = paths::main_worktree(&project_path);
    assert!(!git::branch_exists(&main_repo, "login").unwrap());
    // State removed
    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn merge_already_merged_blocks_cleanup_on_dirty_worktree() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    TestServer::add_feature_commit(&project_path, "login");

    // First merge with --keep to set status to Merged
    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
        server.name(),
    )
    .unwrap();

    // Add uncommitted changes to the feature worktree
    let worktree = project_path.join("login");
    std::fs::write(worktree.join("dirty.txt"), "uncommitted").unwrap();
    git::stage_file(&worktree, "dirty.txt").unwrap();

    // Second merge without --keep should fail due to uncommitted changes
    let result = feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("uncommitted changes")
    );
}

#[test]
fn merge_tolerates_missing_session() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let project_name = server.scope("myapp");
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    TestServer::add_feature_commit(&project_path, "login");

    // Kill the session before merging
    tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    )
    .unwrap();

    // Everything still cleaned up
    assert!(!project_path.join("login").exists());
    let main_repo = paths::main_worktree(&project_path);
    assert!(!git::branch_exists(&main_repo, "login").unwrap());
    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn a_merge_whose_worktree_cannot_be_removed_still_ends_the_feature() {
    use crate::testing::{lock_in, unlock};
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
    TestServer::add_feature_commit(&project_path, "login");
    let locked = lock_in(&project_path.join("login"));
    let projects_dir = TestServer::registry_dir(&project_path);

    let Ended { warnings, .. } =
        feat_merge(&project_path, &projects_dir, "login", false, server.name()).unwrap();

    assert!(
        warnings.iter().any(|w| w.contains("could not remove")),
        "{warnings:?}"
    );
    let main = paths::main_worktree(&project_path);
    assert!(main.join("feature.txt").exists(), "the merge landed");
    assert!(!git::branch_exists(&main, "login").unwrap());
    assert!(!FeatureState::exists(
        &paths::features_dir(&project_path),
        "login"
    ));
    unlock(&locked);
}

#[test]
fn a_merge_whose_cleanup_failed_is_recorded_merged_so_a_retry_only_cleans_up() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
    TestServer::add_feature_commit(&project_path, "login");
    let main = paths::main_worktree(&project_path);
    let holder = dir.path().join("holder");
    git::run_git(
        &main,
        &[
            "worktree",
            "add",
            "--force",
            &holder.to_string_lossy(),
            "login",
        ],
    )
    .unwrap();
    let projects_dir = TestServer::registry_dir(&project_path);
    let features_dir = paths::features_dir(&project_path);

    let failed = feat_merge(&project_path, &projects_dir, "login", false, server.name());

    assert!(failed.is_err(), "the branch is checked out elsewhere");
    assert!(main.join("feature.txt").exists(), "the merge landed");
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.status, FeatureStatus::Merged);

    git::remove_worktree_force(&main, &holder).unwrap();
    feat_merge(&project_path, &projects_dir, "login", false, server.name()).unwrap();

    assert!(!FeatureState::exists(&features_dir, "login"));
    assert!(!git::branch_exists(&main, "login").unwrap());
}
