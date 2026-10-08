//! `blocker`: the reason a device's Merge is off, which follows the CLI's
//! refusals and adds a branch behind its base.

use super::*;
use crate::git::run_git;

fn commit(repo: &Path, file: &str) {
    std::fs::write(repo.join(file), file).unwrap();
    git::stage_file(repo, file).unwrap();
    git::commit(repo, file).unwrap();
}

#[test]
fn a_device_is_told_why_merge_is_off_whenever_the_cli_would_refuse_or_the_branch_is_behind() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
    let registry = TestServer::registry_dir(&project);
    let feature = project.join("login");
    let main = paths::main_worktree(&project);
    let blocker = || super::blocker(&project, &registry, "login").unwrap();
    let refusal = || {
        feat_merge(&project, &registry, "login", true, server.name())
            .unwrap_err()
            .to_string()
    };
    TestServer::add_feature_commit(&project, "login");
    assert_eq!(blocker(), None);

    std::fs::write(feature.join("draft.txt"), "draft").unwrap();
    git::stage_file(&feature, "draft.txt").unwrap();
    assert_eq!(blocker().as_deref(), Some("Uncommitted changes"));
    assert!(refusal().contains("feature 'login' has uncommitted changes"));
    run_git(&feature, &["rm", "-qf", "draft.txt"]).unwrap();

    commit(&main, "upstream.txt");
    assert_eq!(blocker().as_deref(), Some("Behind main: rebase first"));
    TestServer::pause_rebase(&feature, "main");
    assert_eq!(blocker().as_deref(), Some("Rebase in progress"));
    run_git(&feature, &["rebase", "--abort"]).unwrap();
    run_git(&feature, &["rebase", "-q", "main"]).unwrap();
    assert_eq!(blocker(), None);

    git::create_branch_from(&main, "side", "main").unwrap();
    let side = dir.path().join("side");
    git::add_worktree(&main, &side, "side").unwrap();
    commit(&side, "side.txt");
    run_git(&main, &["merge", "--no-ff", "--no-commit", "side"]).unwrap();
    assert_eq!(blocker().as_deref(), Some("Merge in progress in main"));
    assert!(refusal().contains("main worktree has a merge in progress"));
}

#[test]
fn a_device_is_told_when_the_base_is_gone() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, registry) = server.setup_orphaned_child(dir.path());

    assert_eq!(
        super::blocker(&project, &registry, "child")
            .unwrap()
            .as_deref(),
        Some("Base parent is gone")
    );
}

#[test]
fn a_feature_already_merged_only_cleans_up_so_is_never_behind() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
    let registry = TestServer::registry_dir(&project);
    TestServer::add_feature_commit(&project, "login");
    commit(&paths::main_worktree(&project), "upstream.txt");
    assert!(
        super::blocker(&project, &registry, "login")
            .unwrap()
            .is_some()
    );

    let features_dir = paths::features_dir(&project);
    let mut state = FeatureState::load(&features_dir, "login").unwrap();
    state.status = FeatureStatus::Merged;
    state.save(&features_dir, "login").unwrap();

    assert_eq!(super::blocker(&project, &registry, "login").unwrap(), None);
}
