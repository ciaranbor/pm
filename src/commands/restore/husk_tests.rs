//! Restoring a root that holds `.pm/` state but no main checkout.

use super::*;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::project::ProjectConfig;
use crate::testing::TestServer;
use crate::tmux;
use chrono::Utc;
use tempfile::{TempDir, tempdir};

/// A root holding only `.pm/messages`, with `.pm/` a git repo when
/// `state_repo`.
fn husk(root: &Path, state_repo: bool) {
    let pm_dir = paths::pm_dir(root);
    std::fs::create_dir_all(pm_dir.join("messages/main/implementer")).unwrap();
    std::fs::write(pm_dir.join("messages/main/implementer/x.md"), "lost").unwrap();
    if state_repo {
        git::init_repo(&pm_dir).unwrap();
    }
}

/// A bare repo with one commit on `main`, and a `feat-login` branch when
/// `feature`.
fn code_remote(dir: &Path, feature: bool) -> PathBuf {
    let bare = dir.join("code.git");
    git::init_bare(&bare).unwrap();
    let staging = dir.join("staging");
    git::init_repo(&staging).unwrap();
    git::add_remote(&staging, "origin", &bare.to_string_lossy()).unwrap();
    git::push(&staging, "origin", "main").unwrap();
    if feature {
        git::create_branch(&staging, "feat-login").unwrap();
        git::push(&staging, "origin", "feat-login").unwrap();
    }
    bare
}

fn register(
    projects_dir: &Path,
    name: &str,
    root: &Path,
    repo: Option<&Path>,
    state: Option<&Path>,
) {
    let url = |p: &Path| p.to_string_lossy().into_owned();
    ProjectEntry {
        root: root.to_string_lossy().into_owned(),
        main_branch: "main".to_string(),
        repo_url: repo.map(url),
        state_remote: state.map(url),
    }
    .save(projects_dir, name)
    .unwrap();
}

/// A state repo for project `name` holding its config and an active
/// `login` feature on `feat-login`, pushed to a bare remote.
fn state_remote(dir: &Path, name: &str) -> PathBuf {
    let bare = dir.join("state.git");
    git::init_bare(&bare).unwrap();
    let staging = dir.join("state-staging");
    git::init_repo(&staging).unwrap();
    ProjectConfig {
        project: crate::state::project::ProjectInfo {
            name: name.to_string(),
            max_features: Some(7),
        },
        agents: Default::default(),
        harness: Default::default(),
    }
    .save(&staging)
    .unwrap();
    let now = Utc::now();
    FeatureState {
        status: FeatureStatus::Wip,
        branch: "feat-login".to_string(),
        worktree: "login".to_string(),
        base: "main".to_string(),
        pr: String::new(),
        context: String::new(),
        workflow: None,
        created: now,
        last_active: now,
        progress: Default::default(),
        blocked_reason: None,
        blocked_by: None,
    }
    .save(&staging.join("features"), "login")
    .unwrap();
    git::add_all(&staging).unwrap();
    git::commit_with_message(&staging, "state").unwrap();
    git::add_remote(&staging, "origin", &bare.to_string_lossy()).unwrap();
    let branch = git::current_branch(&staging).unwrap();
    git::push(&staging, "origin", &branch).unwrap();
    bare
}

fn setup() -> (TempDir, PathBuf, TestServer) {
    let dir = tempdir().unwrap();
    let projects_dir = dir.path().join("projects");
    (dir, projects_dir, TestServer::new())
}

#[test]
fn restore_completes_a_husk_from_its_repo_and_state_remote() {
    for state_repo in [true, false] {
        let (dir, projects_dir, server) = setup();
        let name = server.scope("husk");
        let root = dir.path().join(&name);
        husk(&root, state_repo);
        let code = code_remote(dir.path(), true);
        let state = state_remote(dir.path(), &name);
        register(&projects_dir, &name, &root, Some(&code), Some(&state));

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();

        assert!(git::is_git_repo(&paths::main_worktree(&root)), "{msgs:?}");
        let config = ProjectConfig::load(&paths::pm_dir(&root)).unwrap();
        assert_eq!(config.project.max_features, Some(7), "{msgs:?}");
        assert!(root.join("login").is_dir(), "{msgs:?}");
        for scope in ["main", "login"] {
            let session = tmux::session_name(&name, scope);
            assert!(
                tmux::has_session(server.name(), &session).unwrap(),
                "{msgs:?}"
            );
        }
    }
}

#[test]
fn restore_scaffolds_a_husk_without_a_state_remote() {
    let (dir, projects_dir, server) = setup();
    let name = server.scope("husk");
    let root = dir.path().join(&name);
    husk(&root, false);
    let code = code_remote(dir.path(), false);
    register(&projects_dir, &name, &root, Some(&code), None);

    let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();

    let config = ProjectConfig::load(&paths::pm_dir(&root)).unwrap();
    assert_eq!(config.project.name, name, "{msgs:?}");
    assert!(paths::pm_dir(&root).join(".git").is_dir());
    assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());
    let entry = ProjectEntry::load(&projects_dir, &name).unwrap();
    assert_eq!(entry.repo_url, Some(code.to_string_lossy().into_owned()));
}

#[test]
fn restore_skips_a_husk_without_a_repo_url_and_opens_nothing() {
    let (dir, projects_dir, server) = setup();
    let name = server.scope("husk");
    let root = dir.path().join(&name);
    husk(&root, true);
    register(&projects_dir, &name, &root, None, None);

    let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();

    assert!(
        msgs.iter()
            .any(|m| m.contains("holds no main checkout and no repo_url")),
        "{msgs:?}"
    );
    assert!(!tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());
    assert!(!paths::main_worktree(&root).exists());
}

#[test]
fn a_failed_clone_into_a_husk_removes_only_main() {
    let (dir, projects_dir, server) = setup();
    let name = server.scope("husk");
    let root = dir.path().join(&name);
    husk(&root, true);
    let missing = dir.path().join("no-such-repo.git");
    register(&projects_dir, &name, &root, Some(&missing), None);

    let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();

    assert!(msgs.iter().any(|m| m.contains(": error: ")), "{msgs:?}");
    assert!(!paths::main_worktree(&root).exists());
    assert!(
        paths::pm_dir(&root)
            .join("messages/main/implementer/x.md")
            .exists()
    );
}
