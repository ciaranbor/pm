use std::path::Path;

use crate::error::{PmError, Result};
use crate::gh;
use crate::state::feature::FeatureState;
use crate::state::paths;

/// Edit the title and/or body of an existing PR for a feature.
///
/// - Looks up the PR number from feature state
/// - At least one of `title` or `body` must be Some (enforced by clap ArgGroup)
/// - Calls `gh pr edit` to update the PR
/// - Prints confirmation with the PR URL
pub fn feat_pr_edit(
    project_root: &Path,
    name: &str,
    title: Option<&str>,
    body: Option<&str>,
) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let state = FeatureState::load(&features_dir, name)?;

    if state.pr.is_empty() || state.pr == "0" {
        return Err(PmError::Gh(format!(
            "feature '{name}' has no linked PR — run `pm feat pr create` first"
        )));
    }

    let worktree_path = project_root.join(&state.worktree);

    gh::edit_pr(&worktree_path, &state.pr, title, body)?;

    // Fetch the PR URL for confirmation
    let url = match gh::existing_pr(&worktree_path, &state.branch)? {
        Some(pr_ref) => pr_ref.url,
        None => format!("PR #{}", state.pr),
    };

    if title.is_some() {
        eprintln!("Updated PR #{} title", state.pr);
    }
    if body.is_some() {
        eprintln!("Updated PR #{} body", state.pr);
    }
    eprintln!("{url}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::feature::{FeatureState, FeatureStatus};
    use tempfile::tempdir;

    fn make_feature_state(pr: &str) -> FeatureState {
        let now = chrono::Utc::now();
        FeatureState {
            status: FeatureStatus::Wip,
            branch: "test-branch".to_string(),
            worktree: "test".to_string(),
            base: String::new(),
            pr: pr.to_string(),
            context: String::new(),
            created: now,
            last_active: now,
        }
    }

    #[test]
    fn fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let result = feat_pr_edit(dir.path(), "nonexistent", Some("new title"), None);
        assert!(result.is_err());
    }

    #[test]
    fn fails_when_no_pr_linked() {
        let dir = tempdir().unwrap();
        let features_dir = paths::features_dir(dir.path());
        std::fs::create_dir_all(&features_dir).unwrap();

        let state = make_feature_state("");
        state.save(&features_dir, "test").unwrap();

        let result = feat_pr_edit(dir.path(), "test", Some("new title"), None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no linked PR"));
    }

    #[test]
    fn fails_when_pr_is_zero() {
        let dir = tempdir().unwrap();
        let features_dir = paths::features_dir(dir.path());
        std::fs::create_dir_all(&features_dir).unwrap();

        let state = make_feature_state("0");
        state.save(&features_dir, "test").unwrap();

        let result = feat_pr_edit(dir.path(), "test", Some("new title"), None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no linked PR"));
    }

    #[test]
    fn loads_state_and_resolves_worktree_for_linked_pr() {
        // Verify the function correctly loads feature state and resolves
        // the worktree path before attempting the gh call. The gh call
        // itself will fail (no real GitHub repo), but we confirm the
        // pre-gh logic works.
        let dir = tempdir().unwrap();
        let features_dir = paths::features_dir(dir.path());
        std::fs::create_dir_all(&features_dir).unwrap();

        let state = make_feature_state("42");
        state.save(&features_dir, "test").unwrap();

        // Create the worktree directory so path resolution succeeds
        std::fs::create_dir_all(dir.path().join("test")).unwrap();

        let result = feat_pr_edit(dir.path(), "test", Some("new title"), None);
        // Fails at the gh call (no real repo), but the error should be
        // a Gh error (not FeatureNotFound or "no linked PR")
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PmError::Gh(_) | PmError::Io(_)),
            "expected Gh or Io error from gh call, got: {err}"
        );
        // Ensure it's NOT the "no linked PR" error
        assert!(!err.to_string().contains("no linked PR"));
    }
}
