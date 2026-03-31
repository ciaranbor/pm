use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{git, tmux};

/// Rename a feature: update branch, worktree, tmux session, and state file.
pub fn feat_rename(
    project_root: &Path,
    old_name: &str,
    new_name: &str,
    tmux_server: Option<&str>,
) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    // Validate: old feature must exist
    let state = FeatureState::load(&features_dir, old_name)?;

    // Validate: new name must not already exist as a feature
    if FeatureState::exists(&features_dir, new_name) {
        return Err(PmError::FeatureAlreadyExists(new_name.to_string()));
    }

    // Validate: new name must not already exist as a branch
    let main_repo = project_root.join("main");
    if git::branch_exists(&main_repo, new_name)? {
        return Err(PmError::Git(format!("branch '{new_name}' already exists")));
    }

    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let old_worktree_path = project_root.join(&state.worktree);
    let new_worktree_path = project_root.join(new_name);

    // Step 1: Rename git branch
    git::rename_branch(&main_repo, &state.branch, new_name)?;

    // Step 2: Move git worktree
    if old_worktree_path.exists()
        && let Err(e) = git::move_worktree(&main_repo, &old_worktree_path, &new_worktree_path)
    {
        // Rollback branch rename
        let _ = git::rename_branch(&main_repo, new_name, &state.branch);
        return Err(e);
    }

    // Step 3: Rename tmux session
    let old_session = format!("{project_name}/{old_name}");
    let new_session = format!("{project_name}/{new_name}");
    if tmux::has_session(tmux_server, &old_session)?
        && let Err(e) = tmux::rename_session(tmux_server, &old_session, &new_session)
    {
        // Rollback worktree move and branch rename
        let _ = git::move_worktree(&main_repo, &new_worktree_path, &old_worktree_path);
        let _ = git::rename_branch(&main_repo, new_name, &state.branch);
        return Err(e);
    }

    // Step 4: Update state file (save new, delete old)
    let updated = FeatureState {
        branch: new_name.to_string(),
        worktree: new_name.to_string(),
        ..state
    };
    if let Err(e) = updated.save(&features_dir, new_name) {
        // Rollback everything
        let _ = tmux::rename_session(tmux_server, &new_session, &old_session);
        let _ = git::move_worktree(&main_repo, &new_worktree_path, &old_worktree_path);
        let _ = git::rename_branch(&main_repo, new_name, old_name);
        return Err(e);
    }
    // Only delete old state after new one is safely written
    let _ = FeatureState::delete(&features_dir, old_name);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn rename_updates_state_file() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
        assert!(FeatureState::exists(&features_dir, "auth"));

        let state = FeatureState::load(&features_dir, "auth").unwrap();
        assert_eq!(state.branch, "auth");
        assert_eq!(state.worktree, "auth");
    }

    #[test]
    fn rename_updates_git_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", server.name()).unwrap();

        let main_repo = project_path.join("main");
        assert!(!git::branch_exists(&main_repo, "login").unwrap());
        assert!(git::branch_exists(&main_repo, "auth").unwrap());
    }

    #[test]
    fn rename_moves_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", server.name()).unwrap();

        assert!(!project_path.join("login").exists());
        assert!(project_path.join("auth").exists());
        assert!(project_path.join("auth").is_dir());
    }

    #[test]
    fn rename_updates_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", server.name()).unwrap();

        assert!(!tmux::has_session(server.name(), &format!("{project_name}/login")).unwrap());
        assert!(tmux::has_session(server.name(), &format!("{project_name}/auth")).unwrap());
    }

    #[test]
    fn rename_preserves_state_fields() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        let features_dir = paths::features_dir(&project_path);
        let original = FeatureState::load(&features_dir, "login").unwrap();

        feat_rename(&project_path, "login", "auth", server.name()).unwrap();

        let renamed = FeatureState::load(&features_dir, "auth").unwrap();
        assert_eq!(renamed.status, original.status);
        assert_eq!(renamed.context, original.context);
        assert_eq!(renamed.created, original.created);
        assert_eq!(renamed.pr, original.pr);
        assert_eq!(renamed.base, original.base);
    }

    #[test]
    fn rename_nonexistent_feature_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        let result = feat_rename(&project_path, "nonexistent", "new-name", server.name());
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn rename_to_existing_feature_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        feat_new::feat_new(
            &project_path,
            "signup",
            None,
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        let result = feat_rename(&project_path, "login", "signup", server.name());
        assert!(matches!(
            result.unwrap_err(),
            PmError::FeatureAlreadyExists(_)
        ));

        // Original feature should be untouched
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn rename_to_existing_branch_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Create a branch without a feature
        let main_repo = project_path.join("main");
        git::create_branch(&main_repo, "taken-branch").unwrap();

        let result = feat_rename(&project_path, "login", "taken-branch", server.name());
        assert!(result.is_err());

        // Original feature should be untouched
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(git::branch_exists(&main_repo, "login").unwrap());
    }
}
