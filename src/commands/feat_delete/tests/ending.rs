use super::*;

#[test]
fn delete_collects_a_legacy_summary_md_for_main() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
    std::fs::write(
        project_path.join("login/summary.md"),
        "Feature notes here.\n",
    )
    .unwrap();

    delete(&server, &project_path, false);

    assert_eq!(
        std::fs::read_to_string(paths::summary_path(&project_path, "login")).unwrap(),
        "Feature notes here.\n"
    );
    let inbox = main_inbox(&project_path);
    assert_eq!(inbox.len(), 1);
    assert!(
        inbox[0].contains(
            &paths::summary_path(&project_path, "login")
                .display()
                .to_string()
        ),
        "{inbox:?}"
    );
}

#[test]
fn delete_of_a_merged_ready_feature_main_has_read_keeps_its_summary_and_notifies_main() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
    TestServer::add_feature_commit(&project_path, "login");
    git::run_git(
        &paths::main_worktree(&project_path),
        &["merge", "--no-edit", "login"],
    )
    .unwrap();
    std::fs::write(
        crate::commands::feat_summary::path(&project_path, "login").unwrap(),
        "notes",
    )
    .unwrap();
    crate::commands::feat_status::feat_status(
        &project_path,
        "login",
        crate::state::feature::Progress::Ready,
        None,
        Some("implementer"),
    )
    .unwrap();
    messages::next(
        &paths::messages_dir(&project_path),
        "main",
        "main",
        "implementer",
    )
    .unwrap();

    delete(&server, &project_path, false);

    assert!(paths::summary_path(&project_path, "login").exists());
    let inbox = main_inbox(&project_path);
    assert_eq!(inbox.len(), 1, "{inbox:?}");
    assert!(inbox[0].contains("'login' was merged"), "{inbox:?}");
    assert!(
        inbox[0].contains(
            &paths::summary_path(&project_path, "login")
                .display()
                .to_string()
        ),
        "{inbox:?}"
    );
}

#[test]
fn force_delete_of_unmerged_work_tells_main_it_never_landed() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
    TestServer::add_feature_commit(&project_path, "login");

    delete(&server, &project_path, true);

    let inbox = main_inbox(&project_path);
    assert_eq!(inbox.len(), 1, "{inbox:?}");
    assert!(inbox[0].contains("never landed"), "{inbox:?}");
    assert!(!inbox[0].contains("was merged"), "{inbox:?}");
}

#[test]
fn delete_after_merge_keep_tells_main_it_merged_despite_later_commits() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
    let features_dir = paths::features_dir(&project_path);
    let mut state = FeatureState::load(&features_dir, "login").unwrap();
    state.status = FeatureStatus::Merged;
    state.save(&features_dir, "login").unwrap();
    TestServer::add_feature_commit(&project_path, "login");

    delete(&server, &project_path, true);

    let inbox = main_inbox(&project_path);
    assert_eq!(inbox.len(), 1, "{inbox:?}");
    assert!(inbox[0].contains("'login' was merged"), "{inbox:?}");
}

#[test]
fn delete_of_a_feature_without_commits_is_not_reported_merged() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

    delete(&server, &project_path, false);

    let inbox = main_inbox(&project_path);
    assert_eq!(inbox.len(), 1, "{inbox:?}");
    assert!(inbox[0].contains("no commits of its own"), "{inbox:?}");
}

#[test]
fn delete_of_a_renamed_feature_without_commits_is_not_reported_merged() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "auth");
    git::rename_branch(&paths::main_worktree(&project_path), "auth", "login").unwrap();
    let features_dir = paths::features_dir(&project_path);
    let mut state = FeatureState::load(&features_dir, "auth").unwrap();
    state.branch = "login".to_string();
    state.save(&features_dir, "auth").unwrap();

    feat_delete(
        &project_path,
        &TestServer::registry_dir(&project_path),
        "auth",
        false,
        server.name(),
    )
    .unwrap();

    let inbox = main_inbox(&project_path);
    assert_eq!(inbox.len(), 1, "{inbox:?}");
    assert!(inbox[0].contains("no commits of its own"), "{inbox:?}");
}

#[test]
fn delete_without_a_summary_still_tells_main_the_feature_ended() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

    delete(&server, &project_path, false);

    let inbox = main_inbox(&project_path);
    assert_eq!(inbox.len(), 1, "{inbox:?}");
    assert!(inbox[0].contains("no summary"), "{inbox:?}");
    assert!(!paths::summary_path(&project_path, "login").exists());
}
