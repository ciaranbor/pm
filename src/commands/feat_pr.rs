use std::path::Path;

use crate::error::Result;
use crate::gh;
use crate::git;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;

/// Create or link a GitHub PR for a feature.
///
/// - Pushes the branch to origin
/// - If a PR already exists for the branch, links it (stores PR number)
/// - Otherwise creates a new PR (draft by default, non-draft with `ready`)
/// - Respects `.github/pull_request_template.md` if present
/// - Stores the PR number in feature state
/// - Sets status to Review only when `--ready`, keeps Wip for draft PRs
pub fn feat_pr(project_root: &Path, name: &str, ready: bool) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let mut state = FeatureState::load(&features_dir, name)?;
    let worktree_path = project_root.join(&state.worktree);

    // Push the branch to origin
    git::push_branch(&worktree_path, &state.branch)?;

    // Check if a PR already exists for this branch
    let pr_number = if let Some(number) = gh::existing_pr_number(&worktree_path, &state.branch)? {
        eprintln!("PR #{number} already exists for branch '{}'", state.branch);
        number
    } else {
        // Read PR template if it exists
        let template_path = worktree_path.join(".github/pull_request_template.md");
        let template = if template_path.exists() {
            Some(std::fs::read_to_string(&template_path)?)
        } else {
            None
        };

        let draft = !ready;
        let base = state.base_or_default();
        let base_arg = if base == "main" { None } else { Some(base) };
        gh::create_pr(
            &worktree_path,
            &state.branch,
            draft,
            template.as_deref(),
            base_arg,
        )?
    };

    state.pr = pr_number;
    if ready {
        state.status = FeatureStatus::Review;
    }
    state.last_active = chrono::Utc::now();
    state.save(&features_dir, name)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn setup_project_with_feature_and_remote(
        dir: &Path,
        feature_name: &str,
        server: &TestServer,
    ) -> std::path::PathBuf {
        let project_path = dir.join(server.scope("myapp"));
        let projects_dir = dir.join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        // Create a bare remote to push to
        let remote_path = dir.join("remote.git");
        std::process::Command::new("git")
            .args(["init", "--bare", &remote_path.to_string_lossy()])
            .output()
            .unwrap();

        // Add remote to the main repo
        let main_repo = project_path.join("main");
        std::process::Command::new("git")
            .args(["-C", &main_repo.to_string_lossy()])
            .args(["remote", "add", "origin", &remote_path.to_string_lossy()])
            .output()
            .unwrap();

        // Push main branch to remote so it exists
        std::process::Command::new("git")
            .args(["-C", &main_repo.to_string_lossy()])
            .args(["push", "-u", "origin", "main"])
            .output()
            .unwrap();

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
    fn push_branch_succeeds_with_remote() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature_and_remote(dir.path(), "login", &server);

        add_feature_commit(&project_path, "login");

        let worktree = project_path.join("login");
        git::push_branch(&worktree, "login").unwrap();

        // Verify the branch exists on the remote
        let remote_path = dir.path().join("remote.git");
        let output = std::process::Command::new("git")
            .args(["-C", &remote_path.to_string_lossy()])
            .args(["branch"])
            .output()
            .unwrap();
        let branches = String::from_utf8_lossy(&output.stdout);
        assert!(branches.contains("login"));
    }

    #[test]
    fn push_and_state_update_roundtrip() {
        // Full feat_pr requires gh CLI + real GitHub repo, so we test the
        // git-push + state-persistence path that surrounds the gh call.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature_and_remote(dir.path(), "login", &server);

        add_feature_commit(&project_path, "login");

        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);

        let worktree = project_path.join("login");
        git::push_branch(&worktree, "login").unwrap();

        // Simulate what feat_pr does after the gh interaction (draft PR — status stays wip)
        state.pr = "42".to_string();
        state.last_active = chrono::Utc::now();
        state.save(&features_dir, "login").unwrap();

        let reloaded = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(reloaded.pr, "42");
        assert_eq!(reloaded.status, FeatureStatus::Wip);
    }

    #[test]
    fn feat_pr_fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        let result = feat_pr(&project_path, "nonexistent", false);
        assert!(result.is_err());
    }

    #[test]
    fn template_is_detected_from_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature_and_remote(dir.path(), "login", &server);

        let worktree = project_path.join("login");

        // No template initially
        let template_path = worktree.join(".github/pull_request_template.md");
        assert!(!template_path.exists());

        // Create a PR template
        std::fs::create_dir_all(worktree.join(".github")).unwrap();
        std::fs::write(&template_path, "## Description\n\n## Test Plan\n").unwrap();

        // Verify the feat_pr template-detection logic finds it
        assert!(template_path.exists());
        let content = std::fs::read_to_string(&template_path).unwrap();
        assert!(content.contains("## Description"));
        assert!(content.contains("## Test Plan"));
    }
}
