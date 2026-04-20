use std::path::Path;

use crate::commands::feat_delete::{CleanupParams, cleanup_feature};
use crate::error::{PmError, Result};
use crate::git;
use crate::hooks;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectConfig;

/// Merge a feature branch into its base branch from the main worktree.
/// By default, cleans up the feature afterwards (remove worktree, delete branch, remove state, kill session).
/// With `keep`, preserve the feature instead.
pub fn feat_merge(
    project_root: &Path,
    name: &str,
    keep: bool,
    tmux_server: Option<&str>,
) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    let state = FeatureState::load(&features_dir, name)?;
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let base = state.base_or_default();
    let base_repo = project_root.join(base);
    let worktree_path = project_root.join(&state.worktree);

    // Block if already merged
    if state.status == FeatureStatus::Merged {
        return Err(PmError::SafetyCheck(format!(
            "feature '{name}' is already merged"
        )));
    }

    // Block if the feature worktree has uncommitted changes
    if git::has_uncommitted_changes(&worktree_path)? {
        return Err(PmError::SafetyCheck(format!(
            "feature '{name}' has uncommitted changes — commit or stash before merging"
        )));
    }

    // Block if the base worktree has uncommitted changes
    if git::has_uncommitted_changes(&base_repo)? {
        return Err(PmError::SafetyCheck(format!(
            "{base} worktree has uncommitted changes — commit or stash before merging"
        )));
    }

    // Check if the branch is already merged locally
    let mut already_merged = git::branch_merged_into(&base_repo, &state.branch, base)?;

    // If not merged locally, check whether the base has an upstream and, if so,
    // fetch and re-check against it. tracking_branch is a local lookup, so
    // checking it first lets us skip the network fetch entirely when there is
    // no upstream. The branch may have been merged upstream (e.g. via GitHub PR).
    if !already_merged
        && let Ok(Some(tracking)) = git::tracking_branch(&base_repo, base)
        && let Ok(()) = git::fetch(&base_repo)
    {
        already_merged = git::branch_merged_into(&base_repo, &state.branch, &tracking)?;
    }

    if already_merged {
        // Branch was merged (locally or upstream) — pull to update local base
        if let Err(e) = git::pull(&base_repo) {
            eprintln!("warning: git pull failed: {e}");
        }
    } else {
        // Perform the merge from the base worktree
        if let Err(e) = git::merge_no_ff(&base_repo, &state.branch) {
            // Abort the failed merge to leave base worktree clean
            if let Err(abort_err) = git::merge_abort(&base_repo) {
                eprintln!("Warning: merge --abort failed: {abort_err}");
            }
            return Err(e);
        }
    }

    // Run post-merge hook in a named "hook" window within the base session
    let hook_path = project_root.join(hooks::POST_MERGE_PATH);
    let base_session = format!("{project_name}/{base}");
    hooks::run_hook(tmux_server, &base_session, &base_repo, &hook_path);

    if keep {
        // Update feature state to Merged
        let mut updated = state.clone();
        updated.status = FeatureStatus::Merged;
        updated.save(&features_dir, name)?;
    } else {
        cleanup_feature(&CleanupParams {
            repo: &base_repo,
            worktree_path: &worktree_path,
            branch: &state.branch,
            features_dir: &features_dir,
            name,
            project_name,
            force_worktree: true, // always force — we already checked for uncommitted changes
            tmux_server,
            delete_branch: true,
            best_effort: false,
            base,
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::hooks;
    use crate::testing::TestServer;
    use crate::tmux as tmux_mod;
    use tempfile::tempdir;

    #[test]
    fn merge_integrates_feature_commits_into_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        TestServer::add_feature_commit(&project_path, "login");

        feat_merge(&project_path, "login", true, server.name()).unwrap();

        // Verify the feature file is now in main
        let main_repo = project_path.join("main");
        assert!(main_repo.join("feature.txt").exists());
    }

    #[test]
    fn merge_creates_merge_commit() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        TestServer::add_feature_commit(&project_path, "login");

        feat_merge(&project_path, "login", true, server.name()).unwrap();

        // Check that the latest commit in main is a merge commit (has two parents)
        let main_repo = project_path.join("main");
        let stdout = git::cat_file(&main_repo, "HEAD").unwrap();
        let parent_count = stdout.lines().filter(|l| l.starts_with("parent ")).count();
        assert_eq!(parent_count, 2, "merge commit should have two parents");
    }

    #[test]
    fn merge_blocks_on_dirty_feature_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Stage a file in the feature worktree (uncommitted change)
        let worktree = project_path.join("login");
        std::fs::write(worktree.join("dirty.txt"), "uncommitted").unwrap();
        git::stage_file(&worktree, "dirty.txt").unwrap();

        let result = feat_merge(&project_path, "login", true, server.name());
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
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Stage a file in the main worktree (uncommitted change)
        let main_repo = project_path.join("main");
        std::fs::write(main_repo.join("dirty.txt"), "uncommitted").unwrap();
        git::stage_file(&main_repo, "dirty.txt").unwrap();

        let result = feat_merge(&project_path, "login", true, server.name());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("uncommitted changes")
        );
    }

    #[test]
    fn merge_cleans_up_by_default() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_name = server.scope("myapp");
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        TestServer::add_feature_commit(&project_path, "login");

        // Verify session exists before merge
        assert!(tmux_mod::has_session(server.name(), &format!("{project_name}/login")).unwrap());

        feat_merge(&project_path, "login", false, server.name()).unwrap();

        // Session killed
        assert!(!tmux_mod::has_session(server.name(), &format!("{project_name}/login")).unwrap());
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
    fn merge_with_keep_leaves_feature_intact() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_name = server.scope("myapp");
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        TestServer::add_feature_commit(&project_path, "login");

        feat_merge(&project_path, "login", true, server.name()).unwrap();

        // Session still exists
        assert!(tmux_mod::has_session(server.name(), &format!("{project_name}/login")).unwrap());
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
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        TestServer::add_feature_commit(&project_path, "login");

        feat_merge(&project_path, "login", true, server.name()).unwrap();

        // Second merge should fail
        let result = feat_merge(&project_path, "login", true, server.name());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already merged"));
    }

    #[test]
    fn merge_conflict_aborts_cleanly() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Create a conflicting file on both main and feature
        let main_repo = project_path.join("main");
        std::fs::write(main_repo.join("shared.txt"), "main content").unwrap();
        git::stage_file(&main_repo, "shared.txt").unwrap();
        git::commit(&main_repo, "main change").unwrap();

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("shared.txt"), "feature content").unwrap();
        git::stage_file(&worktree, "shared.txt").unwrap();
        git::commit(&worktree, "feature change").unwrap();

        let result = feat_merge(&project_path, "login", true, server.name());
        assert!(result.is_err());

        // Main worktree should be clean — merge was aborted
        assert!(!git::has_uncommitted_changes(&main_repo).unwrap());

        // State should still be Wip, not Merged
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);
    }

    #[test]
    fn merge_skips_local_merge_when_already_merged_upstream() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Simulate the branch being merged upstream by merging it directly in the main worktree via git
        let main_repo = project_path.join("main");
        let worktree = project_path.join("login");
        std::fs::write(worktree.join("feature.txt"), "feature work").unwrap();
        git::stage_file(&worktree, "feature.txt").unwrap();
        git::commit(&worktree, "feature work").unwrap();

        // Merge via git directly (simulating a GitHub PR merge)
        git::merge_no_ff(&main_repo, "login").unwrap();

        // Now pm feat merge should succeed without attempting a redundant merge
        feat_merge(&project_path, "login", false, server.name()).unwrap();

        // Cleanup should have happened
        assert!(!project_path.join("login").exists());
        assert!(!git::branch_exists(&main_repo, "login").unwrap());
    }

    #[test]
    fn merge_detects_remote_merge_and_pulls() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_repo = project_path.join("main");
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
        feat_merge(&project_path, "login", false, server.name()).unwrap();

        // Verify pull brought in the merged changes
        assert!(main_repo.join("feature.txt").exists());
        // Cleanup should have happened
        assert!(!project_path.join("login").exists());
        assert!(!git::branch_exists(&main_repo, "login").unwrap());
    }

    #[test]
    fn merge_tolerates_missing_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_name = server.scope("myapp");
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        TestServer::add_feature_commit(&project_path, "login");

        // Kill the session before merging
        tmux_mod::kill_session(server.name(), &format!("{project_name}/login")).unwrap();

        feat_merge(&project_path, "login", false, server.name()).unwrap();

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
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        let result = feat_merge(&project_path, "nonexistent", true, None);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn merge_runs_default_post_merge_hook_in_main_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_name = server.scope("myapp");
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        TestServer::add_feature_commit(&project_path, "login");

        // Main session should have 1 window before merge
        let before =
            tmux_mod::list_windows(server.name(), &format!("{project_name}/main")).unwrap();
        assert_eq!(before, 1);

        feat_merge(&project_path, "login", true, server.name()).unwrap();

        // Main session should now have 2 windows: original + hook window
        let after = tmux_mod::list_windows(server.name(), &format!("{project_name}/main")).unwrap();
        assert_eq!(after, 2);
        // Hook window should be named "hook"
        let target =
            tmux_mod::find_window(server.name(), &format!("{project_name}/main"), "hook").unwrap();
        assert!(target.is_some());
    }

    #[test]
    fn merge_reuses_existing_hook_window() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_name = server.scope("myapp");
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        TestServer::add_feature_commit(&project_path, "login");
        feat_merge(&project_path, "login", true, server.name()).unwrap();

        // Create a second feature and merge it too
        feat_new::feat_new(
            &project_path,
            "api",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();
        let worktree = project_path.join("api");
        std::fs::write(worktree.join("api.txt"), "api work").unwrap();
        git::stage_file(&worktree, "api.txt").unwrap();
        git::commit(&worktree, "api work").unwrap();
        feat_merge(&project_path, "api", true, server.name()).unwrap();

        // Should still have just 2 windows — the hook window was reused, not duplicated
        let windows =
            tmux_mod::list_windows(server.name(), &format!("{project_name}/main")).unwrap();
        assert_eq!(windows, 2);
    }

    #[test]
    fn merge_skips_hook_when_script_removed() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_name = server.scope("myapp");
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Remove the bootstrapped hook script
        std::fs::remove_file(project_path.join(hooks::POST_MERGE_PATH)).unwrap();

        TestServer::add_feature_commit(&project_path, "login");
        feat_merge(&project_path, "login", true, server.name()).unwrap();

        // Main session should still have just 1 window
        let windows =
            tmux_mod::list_windows(server.name(), &format!("{project_name}/main")).unwrap();
        assert_eq!(windows, 1);
    }

    #[test]
    fn merge_hook_succeeds_when_main_session_absent() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_name = server.scope("myapp");
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        TestServer::add_feature_commit(&project_path, "login");

        // Kill the main session before merging
        tmux_mod::kill_session(server.name(), &format!("{project_name}/main")).unwrap();

        // Merge should still succeed — hook skip is non-fatal
        feat_merge(&project_path, "login", true, server.name()).unwrap();

        // Verify the merge itself worked
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Merged);
    }

    #[test]
    fn merge_stacked_feature_merges_into_parent_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "parent");

        // Add a commit to parent
        let parent_wt = project_path.join("parent");
        std::fs::write(parent_wt.join("parent.txt"), "parent work").unwrap();
        git::stage_file(&parent_wt, "parent.txt").unwrap();
        git::commit(&parent_wt, "parent commit").unwrap();

        // Create stacked feature based on parent
        feat_new::feat_new(
            &project_path,
            "child",
            None,
            None,
            Some("parent"),
            false,
            None,
            server.name(),
        )
        .unwrap();
        let child_wt = project_path.join("child");
        std::fs::write(child_wt.join("child.txt"), "child work").unwrap();
        git::stage_file(&child_wt, "child.txt").unwrap();
        git::commit(&child_wt, "child commit").unwrap();

        feat_merge(&project_path, "child", true, server.name()).unwrap();

        // Child's changes should appear in parent worktree, not main
        assert!(parent_wt.join("child.txt").exists());
        let main_repo = project_path.join("main");
        assert!(!main_repo.join("child.txt").exists());
    }

    #[test]
    fn merge_stacked_feature_cleans_up_by_default() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "parent");

        feat_new::feat_new(
            &project_path,
            "child",
            None,
            None,
            Some("parent"),
            false,
            None,
            server.name(),
        )
        .unwrap();
        let child_wt = project_path.join("child");
        std::fs::write(child_wt.join("child.txt"), "child work").unwrap();
        git::stage_file(&child_wt, "child.txt").unwrap();
        git::commit(&child_wt, "child commit").unwrap();

        feat_merge(&project_path, "child", false, server.name()).unwrap();

        // Cleaned up
        assert!(!project_path.join("child").exists());
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "child"));
    }
}
