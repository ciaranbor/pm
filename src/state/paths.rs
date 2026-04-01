use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};

const PM_DIR_NAME: &str = ".pm";
const FEATURES_DIR_NAME: &str = "features";
const CONFIG_DIR_NAME: &str = "pm";
const PROJECTS_DIR_NAME: &str = "projects";

/// Returns the global config directory: ~/.config/pm/
pub fn global_config_dir() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().ok_or_else(|| {
        PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not determine config directory",
        ))
    })?;
    Ok(config_dir.join(CONFIG_DIR_NAME))
}

/// Returns the global projects registry directory: ~/.config/pm/projects/
pub fn global_projects_dir() -> Result<PathBuf> {
    Ok(global_config_dir()?.join(PROJECTS_DIR_NAME))
}

/// Returns the .pm/ directory for a given project root.
pub fn pm_dir(project_root: &Path) -> PathBuf {
    project_root.join(PM_DIR_NAME)
}

/// Returns the features state directory for a given project root.
pub fn features_dir(project_root: &Path) -> PathBuf {
    pm_dir(project_root).join(FEATURES_DIR_NAME)
}

/// Walk up from `start` to find the project root (directory containing `.pm/`).
/// Returns `None` if no `.pm/` directory is found.
pub fn find_project_root(start: &Path) -> Result<PathBuf> {
    let mut current = start.to_path_buf();

    // Canonicalize to resolve symlinks and get absolute path
    if current.is_relative() {
        current = std::env::current_dir()?.join(current);
    }
    current = current.canonicalize()?;

    loop {
        if current.join(PM_DIR_NAME).is_dir() {
            return Ok(current);
        }
        if !current.pop() {
            return Err(PmError::NotInProject);
        }
    }
}

/// Returns the first path component of `cwd` relative to `project_root`.
/// e.g. if project_root is `/a/b` and cwd is `/a/b/main/src`, returns `Some("main")`.
fn first_relative_component(project_root: &Path, cwd: &Path) -> Option<String> {
    let cwd = cwd.canonicalize().ok()?;
    let root = project_root.canonicalize().ok()?;
    let relative = cwd.strip_prefix(&root).ok()?;
    relative
        .components()
        .next()?
        .as_os_str()
        .to_str()
        .map(|s| s.to_string())
}

/// Returns true if CWD is inside the main worktree (`<project_root>/main/`).
pub fn is_in_main_worktree(project_root: &Path, cwd: &Path) -> bool {
    first_relative_component(project_root, cwd).as_deref() == Some("main")
}

/// Detect the current feature name from the working directory.
/// Returns the feature name if CWD is inside a known feature worktree, None otherwise.
pub fn detect_feature_from_cwd(project_root: &Path, cwd: &Path) -> Option<String> {
    let name = first_relative_component(project_root, cwd)?;
    if name == "main" || name == PM_DIR_NAME {
        return None;
    }
    let feat_dir = features_dir(project_root);
    if !crate::state::feature::FeatureState::exists(&feat_dir, &name) {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn pm_dir_returns_correct_path() {
        let root = Path::new("/home/user/projects/myapp");
        assert_eq!(pm_dir(root), PathBuf::from("/home/user/projects/myapp/.pm"));
    }

    #[test]
    fn features_dir_returns_correct_path() {
        let root = Path::new("/home/user/projects/myapp");
        assert_eq!(
            features_dir(root),
            PathBuf::from("/home/user/projects/myapp/.pm/features")
        );
    }

    #[test]
    fn find_project_root_from_root_itself() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let found = find_project_root(root).unwrap();
        assert_eq!(found, root.canonicalize().unwrap());
    }

    #[test]
    fn find_project_root_from_worktree_subdirectory() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        // Simulate a worktree subdirectory: <root>/main/src/
        let deep = root.join("main").join("src");
        std::fs::create_dir_all(&deep).unwrap();

        let found = find_project_root(&deep).unwrap();
        assert_eq!(found, root.canonicalize().unwrap());
    }

    #[test]
    fn find_project_root_outside_project_returns_error() {
        let dir = tempdir().unwrap();
        // No .pm/ directory anywhere
        let result = find_project_root(dir.path());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::NotInProject));
    }

    fn create_feature_state(root: &Path, name: &str) {
        let feat_dir = root.join(".pm").join("features");
        std::fs::create_dir_all(&feat_dir).unwrap();
        std::fs::write(feat_dir.join(format!("{name}.toml")), "").unwrap();
    }

    #[test]
    fn detect_feature_from_feature_worktree() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();
        create_feature_state(root, "login");
        let feature_dir = root.join("login").join("src");
        std::fs::create_dir_all(&feature_dir).unwrap();

        let result = detect_feature_from_cwd(root, &feature_dir);
        assert_eq!(result, Some("login".to_string()));
    }

    #[test]
    fn detect_feature_from_feature_root_dir() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();
        create_feature_state(root, "login");
        let feature_dir = root.join("login");
        std::fs::create_dir_all(&feature_dir).unwrap();

        let result = detect_feature_from_cwd(root, &feature_dir);
        assert_eq!(result, Some("login".to_string()));
    }

    #[test]
    fn detect_feature_returns_none_for_unknown_directory() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();
        // No feature state for "docs"
        let docs_dir = root.join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();

        let result = detect_feature_from_cwd(root, &docs_dir);
        assert_eq!(result, None);
    }

    #[test]
    fn detect_feature_returns_none_in_main() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();
        let main_dir = root.join("main").join("src");
        std::fs::create_dir_all(&main_dir).unwrap();

        let result = detect_feature_from_cwd(root, &main_dir);
        assert_eq!(result, None);
    }

    #[test]
    fn detect_feature_returns_none_in_pm_dir() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = detect_feature_from_cwd(root, &root.join(".pm"));
        assert_eq!(result, None);
    }

    #[test]
    fn detect_feature_returns_none_at_project_root() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = detect_feature_from_cwd(root, root);
        assert_eq!(result, None);
    }

    #[test]
    fn detect_feature_returns_none_outside_project() {
        let project_dir = tempdir().unwrap();
        let other_dir = tempdir().unwrap();
        let root = project_dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = detect_feature_from_cwd(root, other_dir.path());
        assert_eq!(result, None);
    }

    #[test]
    fn is_in_main_worktree_true_in_main() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let main_dir = root.join("main").join("src");
        std::fs::create_dir_all(&main_dir).unwrap();

        assert!(is_in_main_worktree(root, &main_dir));
    }

    #[test]
    fn is_in_main_worktree_true_at_main_root() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let main_dir = root.join("main");
        std::fs::create_dir_all(&main_dir).unwrap();

        assert!(is_in_main_worktree(root, &main_dir));
    }

    #[test]
    fn is_in_main_worktree_false_in_feature() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let feature_dir = root.join("login");
        std::fs::create_dir_all(&feature_dir).unwrap();

        assert!(!is_in_main_worktree(root, &feature_dir));
    }

    #[test]
    fn is_in_main_worktree_false_at_project_root() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        assert!(!is_in_main_worktree(root, root));
    }

    #[test]
    fn global_config_dir_returns_path_under_config() {
        let config = global_config_dir().unwrap();
        assert!(config.ends_with("pm"));
    }

    #[test]
    fn global_projects_dir_returns_path_under_config() {
        let projects = global_projects_dir().unwrap();
        assert!(projects.ends_with("pm/projects"));
    }
}
