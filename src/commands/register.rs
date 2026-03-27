use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::paths;
use crate::state::project::{GithubConfig, ProjectConfig, ProjectEntry, ProjectInfo, SetupConfig};

/// Register an existing git repo as a pm project.
///
/// Creates a wrapper directory (`<name>-pm/`) and either symlinks or moves
/// the original repo as `main/` inside it. Sets up the `.pm/` state directory.
pub fn register(
    repo_path: &Path,
    name: Option<&str>,
    projects_dir: &Path,
    move_repo: bool,
) -> Result<()> {
    // Validate the repo path exists and is a git repo
    let repo_path = repo_path.canonicalize().map_err(|_| {
        PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("path does not exist: {}", repo_path.display()),
        ))
    })?;

    if !crate::git::is_git_repo(&repo_path) {
        return Err(PmError::NotAGitRepo(repo_path.to_path_buf()));
    }

    // Determine project name
    let project_name = name
        .map(|n| n.to_string())
        .or_else(|| {
            repo_path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_string())
        })
        .ok_or_else(|| {
            PmError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "could not determine project name from path",
            ))
        })?;

    // Create wrapper directory
    let parent = repo_path.parent().ok_or_else(|| {
        PmError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "repo path has no parent directory",
        ))
    })?;
    let wrapper_dir = parent.join(format!("{project_name}-pm"));

    if wrapper_dir.exists() {
        return Err(PmError::PathAlreadyExists(wrapper_dir));
    }

    std::fs::create_dir_all(&wrapper_dir)?;

    // Place the repo as main/ inside the wrapper
    let main_path = wrapper_dir.join("main");
    if move_repo {
        std::fs::rename(&repo_path, &main_path)?;
    } else {
        #[cfg(unix)]
        std::os::unix::fs::symlink(&repo_path, &main_path)?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&repo_path, &main_path)?;
    }

    // Create .pm/ structure
    let pm_dir = paths::pm_dir(&wrapper_dir);
    let features_dir = paths::features_dir(&wrapper_dir);
    std::fs::create_dir_all(&features_dir)?;

    // Write project config
    let config = ProjectConfig {
        project: ProjectInfo {
            name: project_name.clone(),
        },
        setup: SetupConfig::default(),
        github: GithubConfig::default(),
    };
    config.save(&pm_dir)?;

    // Register in global registry
    let entry = ProjectEntry {
        root: wrapper_dir.to_string_lossy().to_string(),
        main_branch: "main".to_string(),
    };
    entry.save(projects_dir, &project_name)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use tempfile::tempdir;

    fn create_git_repo(path: &Path) {
        git::init_repo(path).unwrap();
    }

    #[test]
    fn register_creates_wrapper_directory() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myapp");
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false).unwrap();

        let wrapper = dir.path().join("myapp-pm");
        assert!(wrapper.exists());
        assert!(wrapper.is_dir());
    }

    #[test]
    fn register_symlink_creates_symlink_to_original_repo() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myapp");
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false).unwrap();

        let symlink = dir.path().join("myapp-pm").join("main");
        assert!(symlink.exists());
        assert!(symlink.is_symlink());
        assert_eq!(
            symlink.canonicalize().unwrap(),
            repo_path.canonicalize().unwrap()
        );
        // Original repo still exists
        assert!(repo_path.exists());
    }

    #[test]
    fn register_move_moves_repo_into_wrapper() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myapp");
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, true).unwrap();

        let main_path = dir.path().join("myapp-pm").join("main");
        assert!(main_path.exists());
        assert!(main_path.join(".git").exists());
        assert!(!main_path.is_symlink());
        // Original repo is gone
        assert!(!repo_path.exists());
    }

    #[test]
    fn register_creates_pm_structure() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myapp");
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false).unwrap();

        let wrapper = dir.path().join("myapp-pm");
        assert!(wrapper.join(".pm").exists());
        assert!(wrapper.join(".pm").join("config.toml").exists());
        assert!(wrapper.join(".pm").join("features").exists());
    }

    #[test]
    fn register_writes_correct_root_in_registry() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myapp");
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false).unwrap();

        let entry = ProjectEntry::load(&projects_dir, "myapp").unwrap();
        // Compare canonicalized paths to handle /var vs /private/var on macOS
        let entry_root = Path::new(&entry.root).canonicalize().unwrap();
        let expected = dir.path().join("myapp-pm").canonicalize().unwrap();
        assert_eq!(entry_root, expected);
    }

    #[test]
    fn register_with_custom_name() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myapp");
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, Some("custom"), &projects_dir, false).unwrap();

        let wrapper = dir.path().join("custom-pm");
        assert!(wrapper.exists());
        assert!(ProjectEntry::load(&projects_dir, "custom").is_ok());
    }

    #[test]
    fn register_non_git_repo_fails() {
        let dir = tempdir().unwrap();
        let not_a_repo = dir.path().join("not-a-repo");
        std::fs::create_dir(&not_a_repo).unwrap();
        let projects_dir = dir.path().join("registry");

        let result = register(&not_a_repo, None, &projects_dir, false);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::NotAGitRepo(_)));
    }

    #[test]
    fn register_when_wrapper_exists_fails() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myapp");
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        // Pre-create the wrapper directory
        std::fs::create_dir(dir.path().join("myapp-pm")).unwrap();

        let result = register(&repo_path, None, &projects_dir, false);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::PathAlreadyExists(_)));
    }

    #[test]
    fn register_same_repo_twice_fails() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("myapp");
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false).unwrap();

        // Second register hits PathAlreadyExists because wrapper dir exists
        let result = register(&repo_path, None, &projects_dir, false);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::PathAlreadyExists(_)));
    }
}
