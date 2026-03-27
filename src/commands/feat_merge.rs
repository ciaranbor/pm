use std::path::Path;

use crate::commands::feat_delete::{CleanupParams, cleanup_feature};
use crate::error::{PmError, Result};
use crate::git;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectConfig;

/// Merge a feature branch into its base branch from the main worktree.
/// With `delete`, clean up the feature afterwards (remove worktree, delete branch, remove state, kill session).
pub fn feat_merge(
    project_root: &Path,
    name: &str,
    delete: bool,
    tmux_server: Option<&str>,
) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    let state = FeatureState::load(&features_dir, name)?;
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let main_repo = project_root.join("main");
    let worktree_path = project_root.join(&state.worktree);

    // Block if already merged
    if state.status == FeatureStatus::Merged {
        return Err(PmError::Git(format!("feature '{name}' is already merged")));
    }

    // Block if the feature worktree has uncommitted changes
    if git::has_uncommitted_changes(&worktree_path)? {
        return Err(PmError::Git(format!(
            "feature '{name}' has uncommitted changes — commit or stash before merging"
        )));
    }

    // Block if the main worktree has uncommitted changes
    if git::has_uncommitted_changes(&main_repo)? {
        return Err(PmError::Git(
            "main worktree has uncommitted changes — commit or stash before merging".to_string(),
        ));
    }

    // Perform the merge from the main worktree
    git::merge_no_ff(&main_repo, &state.branch)?;

    if delete {
        cleanup_feature(&CleanupParams {
            main_repo: &main_repo,
            worktree_path: &worktree_path,
            branch: &state.branch,
            features_dir: &features_dir,
            name,
            project_name,
            force_worktree: true, // always force — we already checked for uncommitted changes
            tmux_server,
        })?;
    } else {
        // Update feature state to Merged
        let mut updated = state.clone();
        updated.status = FeatureStatus::Merged;
        updated.save(&features_dir, name)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::testing::TestServer;
    use crate::tmux as tmux_mod;
    use tempfile::tempdir;

    fn setup_project_with_feature(
        dir: &Path,
        feature_name: &str,
        server: &TestServer,
    ) -> std::path::PathBuf {
        let project_path = dir.join("myapp");
        let projects_dir = dir.join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(&project_path, feature_name, None, server.name()).unwrap();
        project_path
    }

    fn add_feature_commit(project_path: &Path, feature_name: &str) {
        let worktree = project_path.join(feature_name);
        std::fs::write(worktree.join("feature.txt"), "feature work").unwrap();
        std::process::Command::new("git")
            .args(["-C", &worktree.to_string_lossy()])
            .args(["add", "feature.txt"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &worktree.to_string_lossy()])
            .args(["commit", "-m", "feature work"])
            .output()
            .unwrap();
    }

    #[test]
    fn merge_integrates_feature_commits_into_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        add_feature_commit(&project_path, "login");

        feat_merge(&project_path, "login", false, server.name()).unwrap();

        // Verify the feature file is now in main
        let main_repo = project_path.join("main");
        assert!(main_repo.join("feature.txt").exists());
    }

    #[test]
    fn merge_creates_merge_commit() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        add_feature_commit(&project_path, "login");

        feat_merge(&project_path, "login", false, server.name()).unwrap();

        // Check that the latest commit in main is a merge commit (has two parents)
        let main_repo = project_path.join("main");
        let output = std::process::Command::new("git")
            .args(["-C", &main_repo.to_string_lossy()])
            .args(["cat-file", "-p", "HEAD"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parent_count = stdout.lines().filter(|l| l.starts_with("parent ")).count();
        assert_eq!(parent_count, 2, "merge commit should have two parents");
    }

    #[test]
    fn merge_blocks_on_dirty_feature_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Stage a file in the feature worktree (uncommitted change)
        let worktree = project_path.join("login");
        std::fs::write(worktree.join("dirty.txt"), "uncommitted").unwrap();
        std::process::Command::new("git")
            .args(["-C", &worktree.to_string_lossy()])
            .args(["add", "dirty.txt"])
            .output()
            .unwrap();

        let result = feat_merge(&project_path, "login", false, server.name());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("uncommitted changes")
        );
    }

    #[test]
    fn merge_blocks_on_dirty_main_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Stage a file in the main worktree (uncommitted change)
        let main_repo = project_path.join("main");
        std::fs::write(main_repo.join("dirty.txt"), "uncommitted").unwrap();
        std::process::Command::new("git")
            .args(["-C", &main_repo.to_string_lossy()])
            .args(["add", "dirty.txt"])
            .output()
            .unwrap();

        let result = feat_merge(&project_path, "login", false, server.name());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("uncommitted changes")
        );
    }

    #[test]
    fn merge_with_delete_cleans_up() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        add_feature_commit(&project_path, "login");

        // Verify session exists before merge
        assert!(tmux_mod::has_session(server.name(), "myapp/login").unwrap());

        feat_merge(&project_path, "login", true, server.name()).unwrap();

        // Session killed
        assert!(!tmux_mod::has_session(server.name(), "myapp/login").unwrap());
        // Worktree removed
        assert!(!project_path.join("login").exists());
        // Branch deleted
        let main_repo = project_path.join("main");
        assert!(!git::branch_exists(&main_repo, "login").unwrap());
        // State removed
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn merge_without_delete_leaves_feature_intact() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        add_feature_commit(&project_path, "login");

        feat_merge(&project_path, "login", false, server.name()).unwrap();

        // Session still exists
        assert!(tmux_mod::has_session(server.name(), "myapp/login").unwrap());
        // Worktree still exists
        assert!(project_path.join("login").exists());
        // Branch still exists
        let main_repo = project_path.join("main");
        assert!(git::branch_exists(&main_repo, "login").unwrap());
        // State still exists, but status is Merged
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Merged);
    }

    #[test]
    fn merge_already_merged_feature_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        add_feature_commit(&project_path, "login");

        feat_merge(&project_path, "login", false, server.name()).unwrap();

        // Second merge should fail
        let result = feat_merge(&project_path, "login", false, server.name());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already merged"));
    }

    #[test]
    fn merge_conflict_leaves_state_unchanged() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Create a conflicting file on both main and feature
        let main_repo = project_path.join("main");
        std::fs::write(main_repo.join("shared.txt"), "main content").unwrap();
        std::process::Command::new("git")
            .args(["-C", &main_repo.to_string_lossy()])
            .args(["add", "shared.txt"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &main_repo.to_string_lossy()])
            .args(["commit", "-m", "main change"])
            .output()
            .unwrap();

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("shared.txt"), "feature content").unwrap();
        std::process::Command::new("git")
            .args(["-C", &worktree.to_string_lossy()])
            .args(["add", "shared.txt"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &worktree.to_string_lossy()])
            .args(["commit", "-m", "feature change"])
            .output()
            .unwrap();

        let result = feat_merge(&project_path, "login", false, server.name());
        assert!(result.is_err());

        // State should still be Wip, not Merged
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);
    }

    #[test]
    fn merge_with_delete_tolerates_missing_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        add_feature_commit(&project_path, "login");

        // Kill the session before merging
        tmux_mod::kill_session(server.name(), "myapp/login").unwrap();

        feat_merge(&project_path, "login", true, server.name()).unwrap();

        // Everything still cleaned up
        assert!(!project_path.join("login").exists());
        let main_repo = project_path.join("main");
        assert!(!git::branch_exists(&main_repo, "login").unwrap());
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn merge_nonexistent_feature_fails() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        let result = feat_merge(&project_path, "nonexistent", false, None);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }
}
