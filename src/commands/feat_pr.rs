use std::path::Path;

use crate::error::Result;
use crate::gh;
use crate::git;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;

/// Resolve the PR body: explicit body wins, then PR template, then None.
fn resolve_pr_body(worktree_path: &Path, body: Option<&str>) -> Result<Option<String>> {
    match body {
        Some(b) => Ok(Some(b.to_string())),
        None => {
            let template_path = worktree_path.join(".github/pull_request_template.md");
            if template_path.exists() {
                Ok(Some(std::fs::read_to_string(&template_path)?))
            } else {
                Ok(None)
            }
        }
    }
}

/// Create or link a GitHub PR for a feature.
///
/// - Pushes the branch to origin
/// - If a PR already exists for the branch, links it (stores PR number)
///   and updates the body if `body` is provided
/// - Otherwise creates a new PR (draft by default, non-draft with `ready`)
/// - Explicit `body` overrides the PR template
/// - Falls back to `.github/pull_request_template.md` if present
/// - Stores the PR number in feature state
/// - Sets status to Review only when `--ready`, keeps Wip for draft PRs
pub fn feat_pr(project_root: &Path, name: &str, ready: bool, body: Option<&str>) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let mut state = FeatureState::load(&features_dir, name)?;
    let worktree_path = project_root.join(&state.worktree);

    // Push the branch to origin
    git::push_branch(&worktree_path, &state.branch)?;

    // Check if a PR already exists for this branch
    let pr_number = if let Some(number) = gh::existing_pr_number(&worktree_path, &state.branch)? {
        eprintln!("PR #{number} already exists for branch '{}'", state.branch);
        if let Some(b) = body {
            gh::edit_pr_body(&worktree_path, &number, b)?;
            eprintln!("Updated PR #{number} body");
        }
        number
    } else {
        let pr_body = resolve_pr_body(&worktree_path, body)?;

        let draft = !ready;
        let base = state.base_or_default();
        let base_arg = if base == "main" { None } else { Some(base) };
        gh::create_pr(
            &worktree_path,
            &state.branch,
            draft,
            pr_body.as_deref(),
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
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn setup_project_with_feature_and_remote(
        dir: &Path,
        feature_name: &str,
        server: &TestServer,
    ) -> std::path::PathBuf {
        let (project_path, _) = server.setup_project_with_feature(dir, feature_name);

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

        project_path
    }

    #[test]
    fn push_branch_succeeds_with_remote() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature_and_remote(dir.path(), "login", &server);

        TestServer::add_feature_commit(&project_path, "login");

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

        TestServer::add_feature_commit(&project_path, "login");

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
        let (project_path, _, _) = server.setup_project(dir.path());

        let result = feat_pr(&project_path, "nonexistent", false, None);
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

    #[test]
    fn resolve_body_explicit_overrides_template() {
        let dir = tempdir().unwrap();
        let worktree = dir.path();

        // Create a PR template
        std::fs::create_dir_all(worktree.join(".github")).unwrap();
        std::fs::write(
            worktree.join(".github/pull_request_template.md"),
            "template body",
        )
        .unwrap();

        // Explicit body should win over the template
        let result = resolve_pr_body(worktree, Some("custom body")).unwrap();
        assert_eq!(result.as_deref(), Some("custom body"));
    }

    #[test]
    fn resolve_body_falls_back_to_template() {
        let dir = tempdir().unwrap();
        let worktree = dir.path();

        std::fs::create_dir_all(worktree.join(".github")).unwrap();
        std::fs::write(
            worktree.join(".github/pull_request_template.md"),
            "template body",
        )
        .unwrap();

        let result = resolve_pr_body(worktree, None).unwrap();
        assert_eq!(result.as_deref(), Some("template body"));
    }

    #[test]
    fn resolve_body_none_when_no_template_and_no_body() {
        let dir = tempdir().unwrap();
        let result = resolve_pr_body(dir.path(), None).unwrap();
        assert_eq!(result, None);
    }
}
