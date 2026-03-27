use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{git, tmux};

/// Parameters for feature cleanup.
pub struct CleanupParams<'a> {
    pub main_repo: &'a Path,
    pub worktree_path: &'a Path,
    pub branch: &'a str,
    pub features_dir: &'a Path,
    pub name: &'a str,
    pub project_name: &'a str,
    pub force_worktree: bool,
    pub tmux_server: Option<&'a str>,
}

/// Remove a feature's worktree, branch, state file, and tmux session.
///
/// The tmux session is killed last so that cleanup completes even when run
/// from within the feature session (where killing the session would kill
/// this process).
pub fn cleanup_feature(params: &CleanupParams) -> Result<()> {
    // Step 1: Remove git worktree
    if params.worktree_path.exists() {
        if params.force_worktree {
            git::remove_worktree_force(params.main_repo, params.worktree_path)?;
        } else {
            git::remove_worktree(params.main_repo, params.worktree_path)?;
        }
    }

    // Step 2: Delete branch
    if git::branch_exists(params.main_repo, params.branch)? {
        git::delete_branch(params.main_repo, params.branch)?;
    }

    // Step 3: Remove state file
    FeatureState::delete(params.features_dir, params.name)?;

    // Step 4: Kill tmux session (last — see doc comment above)
    let session_name = format!("{}/{}", params.project_name, params.name);
    if tmux::has_session(params.tmux_server, &session_name)? {
        let main_session = format!("{}/main", params.project_name);
        let _ = tmux::switch_client(params.tmux_server, &main_session);
        tmux::kill_session(params.tmux_server, &session_name)?;
    }

    Ok(())
}

/// Safety check results for feature deletion.
pub struct SafetyReport {
    pub has_uncommitted_changes: bool,
    pub untracked_files: Vec<String>,
    pub has_unpushed_commits: bool,
    pub is_merged: bool,
}

impl SafetyReport {
    pub fn is_blocked(&self) -> bool {
        self.has_uncommitted_changes || self.has_unpushed_commits
    }

    pub fn has_warnings(&self) -> bool {
        !self.untracked_files.is_empty()
    }
}

/// Run safety checks on a feature worktree.
/// All checks go through git.rs and propagate errors — a git failure blocks deletion.
pub fn check_safety(
    worktree_path: &Path,
    main_repo: &Path,
    branch: &str,
    main_branch: &str,
) -> Result<SafetyReport> {
    let has_uncommitted_changes = git::has_uncommitted_changes(worktree_path)?;
    let untracked_files = git::untracked_files(worktree_path)?;
    let has_unpushed_commits = git::has_unpushed_commits(worktree_path)?;
    let is_merged = git::branch_merged_into(main_repo, branch, main_branch)?;

    Ok(SafetyReport {
        has_uncommitted_changes,
        untracked_files,
        has_unpushed_commits,
        is_merged,
    })
}

/// Delete a feature: kill session, remove worktree, delete branch, remove state.
pub fn feat_delete(
    project_root: &Path,
    name: &str,
    force: bool,
    tmux_server: Option<&str>,
) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    // Load feature state
    let state = FeatureState::load(&features_dir, name)?;
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let worktree_path = project_root.join(&state.worktree);
    let main_repo = project_root.join("main");

    // Run safety checks unless --force
    if !force {
        let report = check_safety(&worktree_path, &main_repo, &state.branch, "main")?;

        // Always block on uncommitted changes, regardless of merge status
        if report.has_uncommitted_changes {
            return Err(PmError::Git(format!(
                "feature '{name}' has uncommitted changes. Use --force to override."
            )));
        }

        if !report.is_merged {
            // Branch has commits not in main — block
            return Err(PmError::Git(format!(
                "feature '{name}' has commits not merged into main. Use --force to override."
            )));
        }

        // Merged but has unpushed commits (local ahead of upstream)
        if report.has_unpushed_commits {
            return Err(PmError::Git(format!(
                "feature '{name}' has unpushed commits. Use --force to override."
            )));
        }

        if report.has_warnings() {
            eprintln!(
                "warning: feature '{name}' has {} untracked file(s):",
                report.untracked_files.len()
            );
            for f in &report.untracked_files {
                eprintln!("  {f}");
            }
        }
    }

    // Force-remove worktree if --force was passed or if there are untracked files
    // (git worktree remove refuses untracked files without --force, but we've
    // already warned the user about them in the safety checks above)
    let force_worktree = force
        || !git::untracked_files(&worktree_path)
            .unwrap_or_default()
            .is_empty();

    cleanup_feature(&CleanupParams {
        main_repo: &main_repo,
        worktree_path: &worktree_path,
        branch: &state.branch,
        features_dir: &features_dir,
        name,
        project_name,
        force_worktree,
        tmux_server,
    })
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

    #[test]
    fn delete_removes_state_file() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        feat_delete(&project_path, "login", false, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn delete_removes_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        feat_delete(&project_path, "login", false, server.name()).unwrap();

        assert!(!project_path.join("login").exists());
    }

    #[test]
    fn delete_removes_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        feat_delete(&project_path, "login", false, server.name()).unwrap();

        let main_repo = project_path.join("main");
        assert!(!git::branch_exists(&main_repo, "login").unwrap());
    }

    #[test]
    fn delete_removes_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        assert!(tmux_mod::has_session(server.name(), "myapp/login").unwrap());

        feat_delete(&project_path, "login", false, server.name()).unwrap();

        assert!(!tmux_mod::has_session(server.name(), "myapp/login").unwrap());
    }

    #[test]
    fn delete_with_uncommitted_changes_is_blocked() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("test.txt"), "hello").unwrap();
        std::process::Command::new("git")
            .args(["-C", &worktree.to_string_lossy()])
            .args(["add", "test.txt"])
            .output()
            .unwrap();

        let result = feat_delete(&project_path, "login", false, server.name());
        assert!(result.is_err());

        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn delete_with_force_bypasses_safety_checks() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("test.txt"), "hello").unwrap();
        std::process::Command::new("git")
            .args(["-C", &worktree.to_string_lossy()])
            .args(["add", "test.txt"])
            .output()
            .unwrap();

        feat_delete(&project_path, "login", true, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn delete_merged_branch_succeeds_without_force() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Merge the feature branch into main
        let main_repo = project_path.join("main");
        std::process::Command::new("git")
            .args(["-C", &main_repo.to_string_lossy()])
            .args(["merge", "login"])
            .output()
            .unwrap();

        feat_delete(&project_path, "login", false, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn delete_state_persists_if_safety_check_blocks() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("test.txt"), "hello").unwrap();
        std::process::Command::new("git")
            .args(["-C", &worktree.to_string_lossy()])
            .args(["add", "test.txt"])
            .output()
            .unwrap();

        let _ = feat_delete(&project_path, "login", false, server.name());

        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(project_path.join("login").exists());
    }

    #[test]
    fn delete_nonexistent_feature_fails() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        let result = feat_delete(&project_path, "nonexistent", false, None);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn delete_with_untracked_files_still_proceeds() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("untracked.txt"), "hello").unwrap();

        feat_delete(&project_path, "login", false, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn delete_with_unmerged_commits_is_blocked() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("feature.txt"), "content").unwrap();
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

        let result = feat_delete(&project_path, "login", false, server.name());
        assert!(result.is_err());

        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
    }
}
