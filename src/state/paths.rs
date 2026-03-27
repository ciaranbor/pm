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
