use std::path::Path;

use chrono::Utc;

use crate::error::{PmError, Result};
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{git, tmux};

/// Create a new feature: branch + worktree + tmux session + state file.
///
/// The tmux `server` parameter allows tests to use an isolated tmux server.
/// In production, pass `None` to use the default server.
pub fn feat_new(project_root: &Path, name: &str, tmux_server: Option<&str>) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    // Check for duplicate
    if FeatureState::exists(&features_dir, name) {
        return Err(PmError::FeatureAlreadyExists(name.to_string()));
    }

    // Load project config for name
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Step 1: Write state with status = initializing
    let now = Utc::now();
    let mut state = FeatureState {
        status: FeatureStatus::Initializing,
        branch: name.to_string(),
        worktree: name.to_string(),
        base: String::new(),
        pr: String::new(),
        context: String::new(),
        created: now,
        last_active: now,
    };
    state.save(&features_dir, name)?;

    // Step 2: Create git branch
    let main_worktree = project_root.join("main");
    git::create_branch(&main_worktree, name)?;

    // Step 3: Create git worktree
    let worktree_path = project_root.join(name);
    git::add_worktree(&main_worktree, &worktree_path, name)?;

    // Step 4: Create tmux session
    let session_name = format!("{project_name}/{name}");
    tmux::create_session(tmux_server, &session_name, &worktree_path)?;

    // Step 5: Update status to wip
    state.status = FeatureStatus::Wip;
    state.last_active = Utc::now();
    state.save(&features_dir, name)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn setup_project(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let project_path = dir.join("myapp");
        let projects_dir = dir.join("registry");
        init::init(&project_path, &projects_dir).unwrap();
        (project_path, projects_dir)
    }

    #[test]
    fn feat_new_creates_state_file_with_wip_status() {
        let dir = tempdir().unwrap();
        let (project_path, _) = setup_project(dir.path());
        let server = TestServer::new();

        feat_new(&project_path, "login", server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);
    }

    #[test]
    fn feat_new_creates_git_branch() {
        let dir = tempdir().unwrap();
        let (project_path, _) = setup_project(dir.path());
        let server = TestServer::new();

        feat_new(&project_path, "login", server.name()).unwrap();

        let main_path = project_path.join("main");
        assert!(git::branch_exists(&main_path, "login").unwrap());
    }

    #[test]
    fn feat_new_creates_worktree() {
        let dir = tempdir().unwrap();
        let (project_path, _) = setup_project(dir.path());
        let server = TestServer::new();

        feat_new(&project_path, "login", server.name()).unwrap();

        let worktree_path = project_path.join("login");
        assert!(worktree_path.exists());
        assert!(worktree_path.is_dir());
    }

    #[test]
    fn feat_new_creates_tmux_session() {
        let dir = tempdir().unwrap();
        let (project_path, _) = setup_project(dir.path());
        let server = TestServer::new();

        feat_new(&project_path, "login", server.name()).unwrap();

        assert!(tmux::has_session(server.name(), "myapp/login").unwrap());
    }

    #[test]
    fn feat_new_sets_timestamps() {
        let dir = tempdir().unwrap();
        let (project_path, _) = setup_project(dir.path());
        let server = TestServer::new();
        let before = Utc::now();

        feat_new(&project_path, "login", server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert!(state.created >= before);
        assert!(state.last_active >= state.created);
    }

    #[test]
    fn feat_new_state_has_matching_branch_and_worktree() {
        let dir = tempdir().unwrap();
        let (project_path, _) = setup_project(dir.path());
        let server = TestServer::new();

        feat_new(&project_path, "login", server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.branch, "login");
        assert_eq!(state.worktree, "login");
    }

    #[test]
    fn feat_new_duplicate_name_fails() {
        let dir = tempdir().unwrap();
        let (project_path, _) = setup_project(dir.path());
        let server = TestServer::new();

        feat_new(&project_path, "login", server.name()).unwrap();
        let result = feat_new(&project_path, "login", server.name());

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PmError::FeatureAlreadyExists(_)
        ));
    }

    #[test]
    fn feat_new_tmux_failure_leaves_initializing_state_with_orphan_worktree() {
        let dir = tempdir().unwrap();
        let (project_path, _) = setup_project(dir.path());
        let server = TestServer::new();

        // Pre-create a tmux session with the name feat_new will use,
        // so create_session fails with "duplicate session"
        tmux::create_session(server.name(), "myapp/login", dir.path()).unwrap();

        let result = feat_new(&project_path, "login", server.name());
        assert!(result.is_err());

        // State file should exist with initializing status
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Initializing);

        // Branch and worktree were created before tmux failed — orphaned
        let main_path = project_path.join("main");
        assert!(git::branch_exists(&main_path, "login").unwrap());
        assert!(project_path.join("login").exists());
    }

    #[test]
    fn feat_new_worktree_path_conflict_leaves_initializing_state() {
        let dir = tempdir().unwrap();
        let (project_path, _) = setup_project(dir.path());
        let server = TestServer::new();

        // Pre-create the worktree path so add_worktree fails
        std::fs::create_dir(project_path.join("login")).unwrap();
        std::fs::write(project_path.join("login").join("blocker.txt"), "").unwrap();

        let result = feat_new(&project_path, "login", server.name());
        assert!(result.is_err());

        // State file should exist with initializing status
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Initializing);
    }
}
