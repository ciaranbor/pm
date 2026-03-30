use std::path::Path;

use crate::error::{PmError, Result};
use crate::gh;
use crate::git;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;

/// Mark a feature's PR as ready for review.
///
/// - Pushes the branch to origin (to include latest commits)
/// - Calls `gh pr ready` to remove draft status
/// - Sets feature status to Review
pub fn feat_ready(project_root: &Path, name: &str) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let mut state = FeatureState::load(&features_dir, name)?;
    let worktree_path = project_root.join(&state.worktree);

    if state.pr.is_empty() {
        return Err(PmError::Gh(format!(
            "feature '{name}' has no PR — run `pm feat pr` first"
        )));
    }

    // Push latest commits
    git::push_branch(&worktree_path, &state.branch)?;

    // Mark PR as ready on GitHub
    gh::mark_pr_ready(&worktree_path, &state.branch)?;

    state.status = FeatureStatus::Review;
    state.last_active = chrono::Utc::now();
    state.save(&features_dir, name)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn feat_ready_fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        let result = feat_ready(&project_path, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn feat_ready_fails_when_no_pr_linked() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        crate::commands::feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        let result = feat_ready(&project_path, "login");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no PR"));
    }
}
