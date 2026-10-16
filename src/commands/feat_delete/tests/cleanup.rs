use super::*;
use crate::testing::{ControlClient, OwnServer, agent_entry};
use crate::tmux as tmux_mod;

#[test]
fn delete_cleans_up_all_artifacts() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project_with_feature(dir.path(), "login");

    // Pre-create additional artifacts that delete should clean up
    let agents_dir = paths::agents_dir(&project_path);
    let mut registry = crate::state::agent::AgentRegistry::default();
    registry.register(
        "reviewer",
        crate::state::agent::AgentEntry {
            session_id: "test".to_string(),
            ..agent_entry("reviewer")
        },
    );
    registry.save(&agents_dir, "login").unwrap();
    assert!(agents_dir.join("login.toml").exists());

    let messages_dir = paths::messages_dir(&project_path);
    crate::messages::send(&messages_dir, "login", "reviewer", "implementer", "hello").unwrap();
    assert!(messages_dir.join("login").exists());
    let runtime = crate::state::runtime::agent_dir(&project_path, "login", "reviewer").unwrap();

    let scoped_name = server.scope("myapp");
    assert!(
        tmux_mod::has_session(server.name(), &tmux::session_name(&scoped_name, "login")).unwrap()
    );

    feat_delete(&project_path, &projects_dir, "login", false, server.name()).unwrap();

    // State file removed
    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
    // Worktree removed
    assert!(!project_path.join("login").exists());
    // Branch removed
    let main_repo = paths::main_worktree(&project_path);
    assert!(!git::branch_exists(&main_repo, "login").unwrap());
    // Tmux session removed
    assert!(
        !tmux_mod::has_session(server.name(), &tmux::session_name(&scoped_name, "login")).unwrap()
    );
    // Agent registry removed
    assert!(!agents_dir.join("login.toml").exists());
    // Messages removed
    assert!(!messages_dir.join("login").exists());
    assert!(!runtime.exists());
}

#[test]
fn feat_delete_moves_only_the_clients_viewing_the_feature_to_its_base() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, name) =
        server.setup_project_with_feature_no_tmux(dir.path(), "login");
    let own = OwnServer::start("feat-delete-clients");
    for scope in ["main", "login", "other"] {
        tmux::create_session(own.name(), &tmux::session_name(&name, scope), dir.path()).unwrap();
    }
    // The bystander attaches last, so it is the client tmux takes to be
    // current.
    let _viewer = ControlClient::attach(own.name(), &tmux::session_name(&name, "login"));
    let _bystander = ControlClient::attach(own.name(), &tmux::session_name(&name, "other"));

    feat_delete(&project, &projects_dir, "login", false, own.name()).unwrap();

    let mut viewing: Vec<String> = tmux_mod::clients::list(own.name())
        .unwrap()
        .into_iter()
        .map(|c| c.session)
        .collect();
    viewing.sort();
    assert_eq!(
        viewing,
        [
            tmux::session_name(&name, "main"),
            tmux::session_name(&name, "other")
        ]
    );
}

#[test]
fn a_worktree_that_cannot_be_removed_is_reported_and_the_rest_still_goes() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) =
        server.setup_project_with_feature(dir.path(), "login");
    let worktree = project_path.join("login");
    let locked = crate::testing::lock_in(&worktree);

    let Ended { warnings, .. } =
        feat_delete(&project_path, &projects_dir, "login", false, server.name()).unwrap();

    assert!(
        warnings
            .iter()
            .any(|w| w.contains("could not remove") && w.contains("login")),
        "{warnings:?}"
    );
    let main = paths::main_worktree(&project_path);
    assert!(!FeatureState::exists(
        &paths::features_dir(&project_path),
        "login"
    ));
    assert!(!git::branch_exists(&main, "login").unwrap());
    let session = tmux::session_name(&project_name, "login");
    assert!(!tmux::has_session(server.name(), &session).unwrap());
    crate::testing::unlock(&locked);
}

#[test]
fn a_feature_whose_worktree_git_dropped_is_deleted_with_what_is_left_of_it() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project_with_feature(dir.path(), "login");
    let worktree = project_path.join("login");
    let main = paths::main_worktree(&project_path);
    git::unlock_worktree(&main, &worktree);
    std::fs::remove_file(worktree.join(".git")).unwrap();
    git::prune_worktrees(&main).unwrap();
    std::fs::write(worktree.join("left.txt"), "left behind").unwrap();

    let Ended { warnings, .. } =
        feat_delete(&project_path, &projects_dir, "login", false, server.name()).unwrap();

    assert!(
        warnings
            .iter()
            .any(|w| w.contains("no longer a git worktree") && w.contains("login")),
        "{warnings:?}"
    );
    assert!(!worktree.exists());
    assert!(!FeatureState::exists(
        &paths::features_dir(&project_path),
        "login"
    ));
    assert!(!git::branch_exists(&main, "login").unwrap());
}

#[test]
fn a_feature_whose_worktree_directory_is_gone_still_loses_its_branch() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    // Canonical, so the path matches the one git records for the worktree.
    let root = dir.path().canonicalize().unwrap();
    let (project_path, projects_dir, _) = server.setup_project_with_feature(&root, "login");
    let main = paths::main_worktree(&project_path);
    std::fs::remove_dir_all(project_path.join("login")).unwrap();

    feat_delete(&project_path, &projects_dir, "login", false, server.name()).unwrap();

    assert!(!git::branch_exists(&main, "login").unwrap());
    assert!(
        git::find_worktree_for_branch(&main, "login")
            .unwrap()
            .is_none()
    );
}
