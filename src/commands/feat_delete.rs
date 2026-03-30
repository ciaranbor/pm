use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{gh, git, hooks, tmux};

/// Parameters for feature cleanup.
pub struct CleanupParams<'a> {
    pub repo: &'a Path,
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
            git::remove_worktree_force(params.repo, params.worktree_path)?;
        } else {
            git::remove_worktree(params.repo, params.worktree_path)?;
        }
    }

    // Step 2: Delete branch
    if git::branch_exists(params.repo, params.branch)? {
        git::delete_branch(params.repo, params.branch)?;
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

/// Evaluate a safety report and return an error if deletion should be blocked.
/// When `pr_merged` is true, the unmerged-commits and unpushed-commits checks
/// are skipped (handles squash merges where git can't detect the merge).
fn evaluate_safety(report: &SafetyReport, pr_merged: bool, name: &str) -> Result<()> {
    if report.has_uncommitted_changes {
        return Err(PmError::Git(format!(
            "feature '{name}' has uncommitted changes. Use --force to override."
        )));
    }

    if !report.is_merged && !pr_merged {
        return Err(PmError::Git(format!(
            "feature '{name}' has commits not merged into main. Use --force to override."
        )));
    }

    // Skip unpushed check when PR is merged — the commits are on GitHub already
    if report.has_unpushed_commits && !pr_merged {
        return Err(PmError::Git(format!(
            "feature '{name}' has unpushed commits. Use --force to override."
        )));
    }

    Ok(())
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
    let base = state.base_or_default();
    let base_repo = project_root.join(base);

    // Check if the linked PR was merged on GitHub (handles squash merges
    // where git can't detect the merge). Used for both safety bypass and hook.
    let pr_merged =
        !state.pr.is_empty() && gh::pr_is_merged(&base_repo, &state.pr).unwrap_or(false);

    // Run safety checks unless --force
    if !force {
        let report = check_safety(&worktree_path, &base_repo, &state.branch, base)?;
        evaluate_safety(&report, pr_merged, name)?;

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
        repo: &base_repo,
        worktree_path: &worktree_path,
        branch: &state.branch,
        features_dir: &features_dir,
        name,
        project_name,
        force_worktree,
        tmux_server,
    })?;

    // Trigger post-merge hook when deleting a feature whose PR was merged
    if pr_merged {
        let hook_path = project_root.join(hooks::POST_MERGE_PATH);
        let base_session = format!("{project_name}/{base}");
        hooks::run_hook(tmux_server, &base_session, &base_repo, &hook_path);
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
        let project_path = dir.join(server.scope("myapp"));
        let projects_dir = dir.join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(
            &project_path,
            feature_name,
            None,
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();
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

        let scoped_name = server.scope("myapp");
        assert!(tmux_mod::has_session(server.name(), &format!("{scoped_name}/login")).unwrap());

        feat_delete(&project_path, "login", false, server.name()).unwrap();

        assert!(!tmux_mod::has_session(server.name(), &format!("{scoped_name}/login")).unwrap());
    }

    #[test]
    fn delete_with_uncommitted_changes_is_blocked() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("test.txt"), "hello").unwrap();
        git::stage_file(&worktree, "test.txt").unwrap();

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
        git::stage_file(&worktree, "test.txt").unwrap();

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
        git::merge_no_ff(&main_repo, "login").unwrap();

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
        git::stage_file(&worktree, "test.txt").unwrap();

        let _ = feat_delete(&project_path, "login", false, server.name());

        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(project_path.join("login").exists());
    }

    #[test]
    fn delete_nonexistent_feature_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
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
        git::stage_file(&worktree, "feature.txt").unwrap();
        git::commit(&worktree, "feature work").unwrap();

        let result = feat_delete(&project_path, "login", false, server.name());
        assert!(result.is_err());

        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
    }

    // --- evaluate_safety unit tests ---

    fn make_report(uncommitted: bool, merged: bool, unpushed: bool) -> SafetyReport {
        SafetyReport {
            has_uncommitted_changes: uncommitted,
            untracked_files: vec![],
            has_unpushed_commits: unpushed,
            is_merged: merged,
        }
    }

    #[test]
    fn safety_clean_merged_branch_passes() {
        let report = make_report(false, true, false);
        assert!(evaluate_safety(&report, false, "feat").is_ok());
    }

    #[test]
    fn safety_uncommitted_changes_always_blocks() {
        // Blocks even when git-merged
        let report = make_report(true, true, false);
        assert!(evaluate_safety(&report, false, "feat").is_err());

        // Blocks even when PR is merged
        let report = make_report(true, false, false);
        assert!(evaluate_safety(&report, true, "feat").is_err());
    }

    #[test]
    fn safety_unmerged_branch_blocks_without_pr() {
        let report = make_report(false, false, false);
        assert!(evaluate_safety(&report, false, "feat").is_err());
    }

    #[test]
    fn safety_unmerged_branch_passes_when_pr_merged() {
        let report = make_report(false, false, false);
        assert!(evaluate_safety(&report, true, "feat").is_ok());
    }

    #[test]
    fn safety_unpushed_commits_block_without_pr() {
        let report = make_report(false, true, true);
        assert!(evaluate_safety(&report, false, "feat").is_err());
    }

    #[test]
    fn safety_unpushed_commits_pass_when_pr_merged() {
        let report = make_report(false, false, true);
        assert!(evaluate_safety(&report, true, "feat").is_ok());
    }

    #[test]
    fn delete_stacked_feature_merged_into_parent_succeeds() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "parent", &server);

        // Create stacked feature based on parent
        feat_new::feat_new(
            &project_path,
            "child",
            None,
            None,
            Some("parent"),
            false,
            server.name(),
        )
        .unwrap();

        // Merge child into parent so the safety check passes
        let parent_wt = project_path.join("parent");
        git::merge_no_ff(&parent_wt, "child").unwrap();

        // Delete should succeed — child is merged into its base (parent), not main
        feat_delete(&project_path, "child", false, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "child"));
    }

    #[test]
    fn delete_stacked_feature_not_merged_into_parent_blocks() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "parent", &server);

        // Create stacked feature based on parent with a commit
        feat_new::feat_new(
            &project_path,
            "child",
            None,
            None,
            Some("parent"),
            false,
            server.name(),
        )
        .unwrap();
        let child_wt = project_path.join("child");
        std::fs::write(child_wt.join("child.txt"), "child work").unwrap();
        git::stage_file(&child_wt, "child.txt").unwrap();
        git::commit(&child_wt, "child commit").unwrap();

        // Don't merge into parent — should block
        let result = feat_delete(&project_path, "child", false, server.name());
        assert!(result.is_err());

        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "child"));
    }
}
