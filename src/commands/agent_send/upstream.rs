//! `--upstream`: resolving a feature's parent scope, the one holding its
//! base branch.

use std::path::Path;

use crate::commands::feat_delete::MissingBase;
use crate::error::{PmError, Result};
use crate::state::feature::{FeatureState, base_checkout};
use crate::state::paths;

/// Resolve the `--upstream` flag to the scope holding the current feature's
/// base branch. Errors if the current scope is "main" (no parent), if the
/// feature state cannot be loaded, or if the base is checked out nowhere
/// (the parent feature is gone, or the base was never a feature's branch):
/// there is no upstream to guess at.
pub fn resolve_upstream(
    project_root: &Path,
    main_branch: &str,
    current_scope: &str,
) -> Result<String> {
    if current_scope == "main" {
        return Err(PmError::Messaging(
            "--upstream cannot be used from the main scope (there is no parent scope)".to_string(),
        ));
    }
    let features_dir = paths::features_dir(project_root);
    let state = FeatureState::load(&features_dir, current_scope)?;
    let base = state.base_branch(main_branch);
    match base_checkout(project_root, main_branch, base) {
        Ok(checkout) => Ok(checkout.scope),
        Err(PmError::BaseNotCheckedOut(_)) => {
            let missing = MissingBase::probe(&paths::main_worktree(project_root), base)?;
            Err(PmError::Messaging(format!(
                "--upstream has no target for feature '{current_scope}': {}. \
                 Name the scope explicitly: `pm msg send <agent>@<scope>`",
                missing.reason(base)
            )))
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::feature::FeatureStatus;
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn setup_project_minimal(dir: &Path) -> PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        root
    }

    fn save_feature(root: &Path, name: &str, branch: &str, base: &str) {
        let now = Utc::now();
        let state = FeatureState {
            status: FeatureStatus::Wip,
            branch: branch.to_string(),
            worktree: name.to_string(),
            base: base.to_string(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: now,
            last_active: now,
            progress: Default::default(),
            blocked_reason: None,
            blocked_by: None,
        };
        state.save(&root.join(".pm/features"), name).unwrap();
    }

    #[test]
    fn resolve_upstream_from_main_errors() {
        let dir = tempdir().unwrap();
        let root = setup_project_minimal(dir.path());

        let err = resolve_upstream(&root, "main", "main").unwrap_err();
        assert!(format!("{err}").contains("--upstream cannot be used from the main scope"));
    }

    #[test]
    fn resolve_upstream_returns_base_branch() {
        let dir = tempdir().unwrap();
        let root = setup_project_minimal(dir.path());
        save_feature(&root, "login", "login", "main");

        assert_eq!(resolve_upstream(&root, "main", "login").unwrap(), "main");
    }

    #[test]
    fn resolve_upstream_stacked_feature_targets_the_parent_scope() {
        let dir = tempdir().unwrap();
        let root = setup_project_minimal(dir.path());
        save_feature(&root, "login", "me/login", "main");
        save_feature(&root, "login-v2", "login-v2", "me/login");

        assert_eq!(
            resolve_upstream(&root, "main", "login-v2").unwrap(),
            "login"
        );
    }

    #[test]
    fn resolve_upstream_empty_base_targets_main_scope_on_master_project() {
        let dir = tempdir().unwrap();
        let root = setup_project_minimal(dir.path());
        save_feature(&root, "login", "login", "");

        assert_eq!(resolve_upstream(&root, "master", "login").unwrap(), "main");
    }

    #[test]
    fn resolve_upstream_errors_when_base_has_no_checkout() {
        let dir = tempdir().unwrap();
        let root = setup_project_minimal(dir.path());
        let main = paths::main_worktree(&root);
        crate::git::init_repo(&main).unwrap();
        crate::git::create_branch_from(&main, "develop", "main").unwrap();
        save_feature(&root, "child", "child", "parent");
        save_feature(&root, "other", "other", "develop");

        let err = resolve_upstream(&root, "main", "child").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--upstream has no target"), "{msg}");
        assert!(msg.contains("'parent' is gone"), "{msg}");

        let err = resolve_upstream(&root, "main", "other").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("'develop' is not checked out"), "{msg}");
    }
}
