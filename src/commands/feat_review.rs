use std::path::Path;

use chrono::Utc;

use crate::commands::claude_settings;
use crate::error::{PmError, Result};
use crate::gh::PrDetails;
use crate::hooks;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{gh, git, tmux};

/// Check out a PR for review: fetch PR commits, create worktree + tmux session, seed TASK.md.
///
/// The tmux `server` parameter allows tests to use an isolated tmux server.
/// In production, pass `None` to use the default server.
pub fn feat_review(project_root: &Path, pr_arg: &str, tmux_server: Option<&str>) -> Result<String> {
    let main_worktree = project_root.join("main");

    // Fetch PR details from GitHub (gh pr view accepts both numbers and URLs)
    let details = gh::pr_details(&main_worktree, pr_arg)?;
    let feature_name = format!("review-{}", details.number);

    // Fetch PR commits into a local branch via GitHub's pull/<n>/head ref.
    // This works for both same-repo and fork PRs.
    git::fetch_pr(&main_worktree, &details.number, &feature_name)?;

    setup_review(project_root, &details, &feature_name, tmux_server)
}

/// Set up the review feature given an already-available local branch.
/// Separated from the fetch logic so tests can call this directly with a local branch.
fn setup_review(
    project_root: &Path,
    details: &PrDetails,
    feature_name: &str,
    tmux_server: Option<&str>,
) -> Result<String> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);
    let main_worktree = project_root.join("main");

    // Check for duplicate
    if FeatureState::exists(&features_dir, feature_name) {
        return Err(PmError::FeatureAlreadyExists(feature_name.to_string()));
    }

    // Load project config for session naming
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Build TASK.md content from PR details
    let context = format!(
        "Review PR #{}: {}\n{}\n\n{}",
        details.number, details.title, details.url, details.body
    );

    // Step 1: Write state with status = initializing
    let now = Utc::now();
    let mut state = FeatureState {
        status: FeatureStatus::Initializing,
        branch: feature_name.to_string(),
        worktree: feature_name.to_string(),
        base: String::new(),
        pr: details.number.clone(),
        context: context.clone(),
        created: now,
        last_active: now,
    };
    state.save(&features_dir, feature_name)?;

    // Steps 2+: Create resources, rolling back on failure
    let worktree_path = project_root.join(feature_name);
    let session_name = format!("{project_name}/{feature_name}");
    let hook_path = project_root.join(hooks::POST_CREATE_PATH);

    let result: Result<()> = (|| {
        // Step 2: Create git worktree
        git::add_worktree(&main_worktree, &worktree_path, feature_name)?;

        // Step 2.5: Seed Claude Code settings from main worktree
        claude_settings::seed_feature_claude(project_root, &worktree_path)?;

        // Step 2.6: Write TASK.md
        std::fs::write(worktree_path.join("TASK.md"), &context)?;
        git::exclude_pattern(&worktree_path, "TASK.md")?;

        // Step 3: Create tmux session
        tmux::create_session(tmux_server, &session_name, &worktree_path)?;

        // Step 3.5: Open a claude session in a new window to read TASK.md
        let window_target = tmux::new_window(
            tmux_server,
            &session_name,
            &worktree_path,
            Some("claude"),
            false,
        )?;
        tmux::send_keys(tmux_server, &window_target, "claude 'READ TASK.md'")?;

        // Step 3.6: Run post-create hook
        hooks::run_hook(tmux_server, &session_name, &worktree_path, &hook_path);

        // Step 4: Update status to review
        state.status = FeatureStatus::Review;
        state.last_active = Utc::now();
        state.save(&features_dir, feature_name)?;

        Ok(())
    })();

    if let Err(e) = result {
        // Best-effort cleanup — branch is always ours to clean up
        let _ = tmux::kill_session(tmux_server, &session_name);
        if worktree_path.exists() {
            let _ = git::remove_worktree_force(&main_worktree, &worktree_path);
        }
        let _ = git::delete_branch(&main_worktree, feature_name);
        let _ = FeatureState::delete(&features_dir, feature_name);
        return Err(e);
    }

    Ok(feature_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn sample_details() -> PrDetails {
        PrDetails {
            number: "42".to_string(),
            title: "Add login page".to_string(),
            body: "Implements the login page per spec.".to_string(),
            url: "https://github.com/owner/repo/pull/42".to_string(),
            head_ref: "feat-login".to_string(),
        }
    }

    /// Create the local branch that setup_review expects to exist.
    /// In production, git::fetch_pr creates this; in tests we simulate it.
    fn simulate_fetched_pr(project_path: &Path, branch: &str) {
        let main_wt = project_path.join("main");
        git::create_branch(&main_wt, branch).unwrap();
    }

    #[test]
    fn review_creates_state_file_with_review_status() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        let details = sample_details();
        simulate_fetched_pr(&project_path, "review-42");

        setup_review(&project_path, &details, "review-42", server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "review-42").unwrap();
        assert_eq!(state.status, FeatureStatus::Review);
        assert_eq!(state.pr, "42");
        assert_eq!(state.branch, "review-42");
        assert_eq!(state.worktree, "review-42");
    }

    #[test]
    fn review_creates_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        let details = sample_details();
        simulate_fetched_pr(&project_path, "review-42");

        setup_review(&project_path, &details, "review-42", server.name()).unwrap();

        let worktree_path = project_path.join("review-42");
        assert!(worktree_path.exists());
        assert!(worktree_path.is_dir());
    }

    #[test]
    fn review_creates_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        let details = sample_details();
        simulate_fetched_pr(&project_path, "review-42");

        setup_review(&project_path, &details, "review-42", server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &format!("{project_name}/review-42")).unwrap());
    }

    #[test]
    fn review_seeds_task_md() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        let details = sample_details();
        simulate_fetched_pr(&project_path, "review-42");

        setup_review(&project_path, &details, "review-42", server.name()).unwrap();

        let task_md = project_path.join("review-42").join("TASK.md");
        assert!(task_md.exists());
        let content = std::fs::read_to_string(&task_md).unwrap();
        assert!(content.contains("Review PR #42: Add login page"));
        assert!(content.contains("https://github.com/owner/repo/pull/42"));
        assert!(content.contains("Implements the login page per spec."));
    }

    #[test]
    fn review_task_md_is_git_excluded() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        let details = sample_details();
        simulate_fetched_pr(&project_path, "review-42");

        setup_review(&project_path, &details, "review-42", server.name()).unwrap();

        let worktree_path = project_path.join("review-42");
        let untracked = git::untracked_files(&worktree_path).unwrap();
        assert!(!untracked.contains(&"TASK.md".to_string()));
    }

    #[test]
    fn review_creates_claude_and_hook_windows() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        let details = sample_details();
        simulate_fetched_pr(&project_path, "review-42");

        setup_review(&project_path, &details, "review-42", server.name()).unwrap();

        // 3 windows: default shell + claude + hook
        let windows =
            tmux::list_windows(server.name(), &format!("{project_name}/review-42")).unwrap();
        assert_eq!(windows, 3);
    }

    #[test]
    fn review_duplicate_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        let details = sample_details();
        simulate_fetched_pr(&project_path, "review-42");

        setup_review(&project_path, &details, "review-42", server.name()).unwrap();

        let result = setup_review(&project_path, &details, "review-42", server.name());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PmError::FeatureAlreadyExists(_)
        ));
    }

    #[test]
    fn review_sets_timestamps() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        let details = sample_details();
        simulate_fetched_pr(&project_path, "review-42");
        let before = Utc::now();

        setup_review(&project_path, &details, "review-42", server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "review-42").unwrap();
        assert!(state.created >= before);
        assert!(state.last_active >= state.created);
    }

    #[test]
    fn review_tmux_failure_cleans_up() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        let details = sample_details();
        simulate_fetched_pr(&project_path, "review-42");

        // Pre-create the tmux session to force a conflict
        tmux::create_session(
            server.name(),
            &format!("{project_name}/review-42"),
            dir.path(),
        )
        .unwrap();

        let result = setup_review(&project_path, &details, "review-42", server.name());
        assert!(result.is_err());

        // State file should be cleaned up
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "review-42"));

        // Worktree should be cleaned up
        assert!(!project_path.join("review-42").exists());

        // Branch should be cleaned up
        let main_wt = project_path.join("main");
        assert!(!git::branch_exists(&main_wt, "review-42").unwrap());
    }

    #[test]
    fn review_stores_context_in_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        let details = sample_details();
        simulate_fetched_pr(&project_path, "review-42");

        setup_review(&project_path, &details, "review-42", server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "review-42").unwrap();
        assert!(state.context.contains("Review PR #42"));
    }
}
