use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use super::*;
use crate::git::run_git;

#[test]
fn delete_with_uncommitted_changes_is_blocked() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project_with_feature(dir.path(), "login");

    let worktree = project_path.join("login");
    std::fs::write(worktree.join("test.txt"), "hello").unwrap();
    git::stage_file(&worktree, "test.txt").unwrap();

    let result = feat_delete(&project_path, &projects_dir, "login", false, server.name());
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
    let (project_path, projects_dir, _) = server.setup_project_with_feature(dir.path(), "login");

    let worktree = project_path.join("login");
    std::fs::write(worktree.join("test.txt"), "hello").unwrap();
    git::stage_file(&worktree, "test.txt").unwrap();

    feat_delete(&project_path, &projects_dir, "login", true, server.name()).unwrap();

    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn delete_merged_branch_succeeds_without_force() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project_with_feature(dir.path(), "login");

    // Merge the feature branch into main
    let main_repo = paths::main_worktree(&project_path);
    git::merge_no_ff(&main_repo, "login").unwrap();

    feat_delete(&project_path, &projects_dir, "login", false, server.name()).unwrap();

    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn delete_nonexistent_feature_fails() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    let result = feat_delete(&project_path, &projects_dir, "nonexistent", false, None);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
}

#[test]
fn delete_with_untracked_files_still_proceeds() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project_with_feature(dir.path(), "login");

    let worktree = project_path.join("login");
    std::fs::write(worktree.join("untracked.txt"), "hello").unwrap();

    feat_delete(&project_path, &projects_dir, "login", false, server.name()).unwrap();

    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn delete_with_unmerged_commits_is_blocked() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project_with_feature(dir.path(), "login");

    let worktree = project_path.join("login");
    std::fs::write(worktree.join("feature.txt"), "content").unwrap();
    git::stage_file(&worktree, "feature.txt").unwrap();
    git::commit(&worktree, "feature work").unwrap();

    let result = feat_delete(&project_path, &projects_dir, "login", false, server.name());
    assert!(result.is_err());

    let features_dir = paths::features_dir(&project_path);
    assert!(FeatureState::exists(&features_dir, "login"));
}

/// Feature "login", one commit ahead and linked to PR #7, which GitHub
/// (faked) reports merged at that commit; squashed, so git can't tell.
fn merged_pr(
    server: &TestServer,
    dir: &Path,
) -> (PathBuf, PathBuf, crate::gh::fake::Fake, Rc<RefCell<String>>) {
    let (project, projects_dir, _) = server.setup_project_with_feature_no_tmux(dir, "login");
    TestServer::add_feature_commit(&project, "login");
    let (gh, head) = TestServer::link_merged_pr(&project, "login", "7");
    (project, projects_dir, gh, head)
}

#[test]
fn a_branch_wholly_in_its_merged_pr_is_deleted_though_git_sees_it_unmerged() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, _gh, _) = merged_pr(&server, dir.path());

    feat_delete(&project, &projects_dir, "login", false, server.name()).unwrap();

    assert!(!FeatureState::exists(
        &paths::features_dir(&project),
        "login"
    ));
}

#[test]
fn commits_a_merged_pr_lacks_block_the_delete_and_are_named() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, _gh, _) = merged_pr(&server, dir.path());
    let worktree = project.join("login");
    TestServer::commit_file(&worktree, "later.txt", "later work");

    let err = feat_delete(&project, &projects_dir, "login", false, server.name())
        .unwrap_err()
        .to_string();

    assert!(err.contains("PR #7 lacks"), "{err}");
    assert!(err.contains(" later work"), "{err}");
    assert!(worktree.exists());
    assert!(FeatureState::exists(
        &paths::features_dir(&project),
        "login"
    ));
}

#[test]
fn a_merged_pr_head_this_clone_lacks_blocks_the_delete_until_fetched() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, _gh, head) = merged_pr(&server, dir.path());
    let main = paths::main_worktree(&project);
    let elsewhere = dir.path().join("elsewhere");
    run_git(
        dir.path(),
        &[
            "clone",
            "--branch",
            "login",
            &main.to_string_lossy(),
            "elsewhere",
        ],
    )
    .unwrap();
    TestServer::commit_file(&elsewhere, "update.txt", "update branch");
    *head.borrow_mut() = git::branch_commit(&elsewhere, "login").unwrap().unwrap();

    let err = feat_delete(&project, &projects_dir, "login", false, server.name())
        .unwrap_err()
        .to_string();

    assert!(err.contains("not in this clone"), "{err}");
    assert!(project.join("login").exists());

    run_git(
        &main,
        &[
            "fetch",
            &elsewhere.to_string_lossy(),
            "login:refs/remotes/elsewhere/login",
        ],
    )
    .unwrap();
    feat_delete(&project, &projects_dir, "login", false, server.name()).unwrap();

    assert!(!FeatureState::exists(
        &paths::features_dir(&project),
        "login"
    ));
}
