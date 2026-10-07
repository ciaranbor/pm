//! A branch already merged upstream: detected by fetching, then pulled
//! instead of merged.

use super::*;

#[test]
fn merge_skips_local_merge_when_already_merged_upstream() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    // Simulate the branch being merged upstream by merging it directly in the main worktree via git
    let main_repo = paths::main_worktree(&project_path);
    let worktree = project_path.join("login");
    std::fs::write(worktree.join("feature.txt"), "feature work").unwrap();
    git::stage_file(&worktree, "feature.txt").unwrap();
    git::commit(&worktree, "feature work").unwrap();

    // Merge via git directly (simulating a GitHub PR merge)
    git::merge_no_ff(&main_repo, "login").unwrap();

    // Now pm feat merge should succeed without attempting a redundant merge
    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    )
    .unwrap();

    // Cleanup should have happened
    assert!(!project_path.join("login").exists());
    assert!(!git::branch_exists(&main_repo, "login").unwrap());
}

#[test]
fn merge_detects_remote_merge_and_pulls() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

    let main_repo = paths::main_worktree(&project_path);
    let worktree = project_path.join("login");

    // Set up a bare repo as a fake remote
    let remote_path = dir.path().join("remote.git");
    git::init_bare(&remote_path).unwrap();
    git::add_remote(&main_repo, "origin", &remote_path.to_string_lossy()).unwrap();
    git::push(&main_repo, "origin", "main").unwrap();

    // Add a commit on the feature branch and push it to the remote
    // (worktrees share the same git repo, so origin is already configured)
    std::fs::write(worktree.join("feature.txt"), "feature work").unwrap();
    git::stage_file(&worktree, "feature.txt").unwrap();
    git::commit(&worktree, "feature work").unwrap();
    git::push(&worktree, "origin", "login").unwrap();

    // Simulate a GitHub PR merge: clone the remote, merge feature into main, push back
    let scratch = dir.path().join("scratch");
    std::process::Command::new("git")
        .args([
            "clone",
            &remote_path.to_string_lossy(),
            &scratch.to_string_lossy(),
        ])
        .output()
        .unwrap();
    git::merge_no_ff(&scratch, "origin/login").unwrap();
    git::push(&scratch, "origin", "main").unwrap();

    // Local main does NOT have feature.txt — the merge only exists on the remote
    assert!(!main_repo.join("feature.txt").exists());

    // pm feat merge should fetch, detect the remote merge, pull, and clean up
    feat_merge(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "login",
        false,
        server.name(),
    )
    .unwrap();

    // Verify pull brought in the merged changes
    assert!(main_repo.join("feature.txt").exists());
    // Cleanup should have happened
    assert!(!project_path.join("login").exists());
    assert!(!git::branch_exists(&main_repo, "login").unwrap());
}
