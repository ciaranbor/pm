//! Which project and scope a command acts on.

use pm::error::{PmError, Result};
use pm::state::paths;
use pm::state::project::ProjectEntry;
use std::path::{Path, PathBuf};

pub(super) fn resolve_feature_name(name: Option<String>, project_root: &Path) -> Result<String> {
    name.or_else(|| paths::detect_feature_from_cwd(project_root, &std::env::current_dir().ok()?))
        .ok_or(PmError::NotInFeatureWorktree)
}

/// Resolve the current scope: feature name if in a feature worktree,
/// "main" if in the main worktree, error otherwise.
pub(super) fn resolve_scope(project_root: &Path) -> Result<String> {
    paths::resolve_scope_from(project_root, &std::env::current_dir()?)
}

/// Validate that a scope name refers to an existing scope ("main" or a known feature).
fn validate_scope(project_root: &Path, scope: &str) -> Result<()> {
    if scope == "main" {
        return Ok(());
    }
    let features_dir = paths::features_dir(project_root);
    let state_file = features_dir.join(format!("{scope}.toml"));
    if state_file.exists() {
        Ok(())
    } else {
        Err(PmError::FeatureNotFound(scope.to_string()))
    }
}

/// Resolve scope with an optional override flag. If `scope_flag` is Some,
/// validates it and returns it; otherwise auto-detects from CWD.
pub(super) fn resolve_scope_with_flag(
    project_root: &Path,
    scope_flag: Option<String>,
) -> Result<String> {
    match scope_flag {
        Some(s) => {
            validate_scope(project_root, &s)?;
            Ok(s)
        }
        None => resolve_scope(project_root),
    }
}

/// The root of the project named `project`, or else of the one the cwd is in.
/// A named project that isn't on this machine is refused.
pub(super) fn project_root(projects_dir: &Path, project: Option<&str>) -> Result<PathBuf> {
    match project {
        Some(name) => Ok(ProjectEntry::load_here(projects_dir, name)?.1),
        None => paths::find_project_root(&std::env::current_dir()?),
    }
}

/// The current project root, or `None` when the caller isn't inside a
/// project — used by commands that also work outside one. Only that case is
/// swallowed; a genuine I/O failure still propagates.
pub(super) fn optional_project_root() -> Result<Option<PathBuf>> {
    match paths::find_project_root(&std::env::current_dir()?) {
        Ok(root) => Ok(Some(root)),
        Err(PmError::NotInProject) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_feature_state(root: &Path, name: &str) {
        let feat_dir = root.join(".pm").join("features");
        std::fs::create_dir_all(&feat_dir).unwrap();
        std::fs::write(feat_dir.join(format!("{name}.toml")), "").unwrap();
    }

    #[test]
    fn validate_scope_accepts_main() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        assert!(validate_scope(root, "main").is_ok());
    }

    #[test]
    fn validate_scope_accepts_existing_feature() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        create_feature_state(root, "login");

        assert!(validate_scope(root, "login").is_ok());
    }

    #[test]
    fn validate_scope_rejects_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = validate_scope(root, "nonexistent");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn resolve_scope_with_flag_uses_override() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        create_feature_state(root, "login");

        let scope = resolve_scope_with_flag(root, Some("login".to_string())).unwrap();
        assert_eq!(scope, "login");
    }

    #[test]
    fn resolve_scope_with_flag_rejects_invalid_scope() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = resolve_scope_with_flag(root, Some("bogus".to_string()));
        assert!(result.is_err());
    }
}
