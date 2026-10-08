//! Which project and scope a command acts on.

use pm::error::{PmError, Result};
use pm::state::project::ProjectEntry;
use pm::state::{paths, scope};
use std::path::{Path, PathBuf};

pub(super) fn resolve_feature_name(name: Option<String>, project_root: &Path) -> Result<String> {
    name.or_else(|| current_feature(project_root))
        .ok_or(PmError::NotInFeatureWorktree)
}

/// The feature [`resolve_scope`] finds, if it is one.
pub(super) fn current_feature(project_root: &Path) -> Option<String> {
    resolve_scope(project_root)
        .ok()
        .filter(|s| s != scope::MAIN)
}

/// The scope this command acts on ([`paths::command_scope`]): feature name
/// if in a feature worktree, "main" if in the main worktree, else the
/// agent's, error otherwise.
pub(super) fn resolve_scope(project_root: &Path) -> Result<String> {
    paths::command_scope(
        project_root,
        &std::env::current_dir()?,
        paths::agent_worktree().as_deref(),
    )
}

/// Resolve scope with an optional override flag. If `scope_flag` is Some,
/// validates it and returns it; otherwise auto-detects from CWD.
pub(super) fn resolve_scope_with_flag(
    project_root: &Path,
    scope_flag: Option<String>,
) -> Result<String> {
    match scope_flag {
        Some(s) => {
            scope::check(project_root, &s)?;
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
