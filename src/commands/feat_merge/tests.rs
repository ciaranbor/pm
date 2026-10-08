//! `feat_merge` end to end: the merge itself and the checks that refuse it.

mod base;
mod blocker;
mod cleanup;
mod hook;
mod upstream;

use super::*;
use crate::commands::{feat_new, init};
use crate::hooks;
use crate::state::feature::FeatureState;
use crate::testing::TestServer;
use crate::tmux;
use tempfile::tempdir;

#[test]
fn merge_integrates_feature_commits_into_main() {
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

    // Verify the feature file is now in main
    let main_repo = paths::main_worktree(&project_path);
    assert!(main_repo.join("feature.txt").exists());
}

#[test]
fn merge_tells_main_the_feature_merged_and_keeps_its_summary() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
    TestServer::add_feature_commit(&project_path, "login");
    std::fs::write(
        crate::commands::feat_summary::path(&project_path, "login").unwrap(),
        "notes",
    )
    .unwrap();

    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    )
    .unwrap();

    assert!(paths::summary_path(&project_path, "login").exists());
    let msg = crate::messages::read_at(
        &paths::messages_dir(&project_path),
        "main",
        "main",
        "login",
        1,
    )
    .unwrap()
    .unwrap();
    assert!(msg.body.contains("'login' was merged"), "{}", msg.body);
    assert!(
        msg.body.contains(
            &paths::summary_path(&project_path, "login")
                .display()
                .to_string()
        ),
        "{}",
        msg.body
    );
}

#[test]
fn merge_creates_merge_commit() {
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

    // Check that the latest commit in main is a merge commit (has two parents)
    let main_repo = paths::main_worktree(&project_path);
    let stdout = git::cat_file(&main_repo, "HEAD").unwrap();
    let parent_count = stdout.lines().filter(|l| l.starts_with("parent ")).count();
    assert_eq!(parent_count, 2, "merge commit should have two parents");
}

#[test]
fn merge_blocks_on_dirty_feature_worktree() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    // Stage a file in the feature worktree (uncommitted change)
    let worktree = project_path.join("login");
    std::fs::write(worktree.join("dirty.txt"), "uncommitted").unwrap();
    git::stage_file(&worktree, "dirty.txt").unwrap();

    let result = feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
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
fn merge_refuses_a_feature_paused_mid_rebase_with_a_clean_tree() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
    let registry = TestServer::registry_dir(&project_path);
    TestServer::add_feature_commit(&project_path, "login");
    let worktree = project_path.join("login");
    TestServer::pause_rebase(&worktree, "main");

    for keep in [true, false] {
        let err = feat_merge(&project_path, &registry, "login", keep, server.name())
            .unwrap_err()
            .to_string();
        assert!(err.contains("rebase in progress"), "{err}");
    }
    assert!(worktree.exists());
    assert!(
        !paths::main_worktree(&project_path)
            .join("feature.txt")
            .exists()
    );
}

#[test]
fn merge_blocks_on_dirty_main_worktree() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    // Stage a file in the main worktree (uncommitted change)
    let main_repo = paths::main_worktree(&project_path);
    std::fs::write(main_repo.join("dirty.txt"), "uncommitted").unwrap();
    git::stage_file(&main_repo, "dirty.txt").unwrap();

    let result = feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
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
fn merge_conflict_aborts_cleanly() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    // Create a conflicting file on both main and feature
    let main_repo = paths::main_worktree(&project_path);
    std::fs::write(main_repo.join("shared.txt"), "main content").unwrap();
    git::stage_file(&main_repo, "shared.txt").unwrap();
    git::commit(&main_repo, "main change").unwrap();

    let worktree = project_path.join("login");
    std::fs::write(worktree.join("shared.txt"), "feature content").unwrap();
    git::stage_file(&worktree, "shared.txt").unwrap();
    git::commit(&worktree, "feature change").unwrap();

    let result = feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        true,
        server.name(),
    );
    assert!(
        matches!(result, Err(PmError::MergeAborted(_))),
        "{result:?}"
    );

    // Main worktree should be clean — merge was aborted
    assert!(!git::has_uncommitted_changes(&main_repo).unwrap());

    // State should still be Wip, not Merged
    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.status, FeatureStatus::Wip);
}

#[test]
fn merge_nonexistent_feature_fails() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let project_path = dir.path().join(server.scope("myapp"));
    let projects_dir = dir.path().join("registry");
    init::init(&project_path, &projects_dir, None, server.name()).unwrap();

    let result = feat_merge(&project_path, &projects_dir, "nonexistent", true, None);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
}
