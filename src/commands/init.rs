use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;
use crate::state::paths;
use crate::state::project::{GithubConfig, ProjectConfig, ProjectEntry, ProjectInfo, SetupConfig};

/// Initialize a new pm project at the given path.
///
/// Creates:
/// - `<path>/` — project root
/// - `<path>/main/` — main worktree with git init
/// - `<path>/.pm/` — project state directory
/// - `<path>/.pm/config.toml` — project config
/// - `<path>/.pm/features/` — empty features directory
/// - `~/.config/pm/projects/<name>.toml` — global registry entry
pub fn init(path: &Path, projects_dir: &Path) -> Result<()> {
    if path.exists() {
        return Err(PmError::PathAlreadyExists(path.to_path_buf()));
    }

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            PmError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid project path",
            ))
        })?
        .to_string();

    // Create project root
    std::fs::create_dir_all(path)?;

    // Init git repo in main/
    let main_path = path.join("main");
    git::init_repo(&main_path)?;

    // Create .pm/ structure
    let pm_dir = paths::pm_dir(path);
    let features_dir = paths::features_dir(path);
    std::fs::create_dir_all(&features_dir)?;

    // Write project config
    let config = ProjectConfig {
        project: ProjectInfo { name: name.clone() },
        setup: SetupConfig::default(),
        github: GithubConfig::default(),
    };
    config.save(&pm_dir)?;

    // Register in global registry
    let entry = ProjectEntry {
        root: path.to_string_lossy().to_string(),
        main_branch: "main".to_string(),
    };
    entry.save(projects_dir, &name)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_creates_main_directory() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir).unwrap();

        assert!(project_path.join("main").exists());
        assert!(project_path.join("main").is_dir());
    }

    #[test]
    fn init_creates_git_repo_in_main() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir).unwrap();

        assert!(project_path.join("main").join(".git").exists());
    }

    #[test]
    fn init_creates_pm_directory_with_config_and_features() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir).unwrap();

        assert!(project_path.join(".pm").exists());
        assert!(project_path.join(".pm").join("config.toml").exists());
        assert!(project_path.join(".pm").join("features").exists());
    }

    #[test]
    fn init_writes_correct_project_name_in_config() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir).unwrap();

        let pm_dir = project_path.join(".pm");
        let config = ProjectConfig::load(&pm_dir).unwrap();
        assert_eq!(config.project.name, "myapp");
    }

    #[test]
    fn init_registers_project_in_global_registry() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir).unwrap();

        let entry = ProjectEntry::load(&projects_dir, "myapp").unwrap();
        assert_eq!(entry.root, project_path.to_string_lossy().to_string());
        assert_eq!(entry.main_branch, "main");
    }

    #[test]
    fn init_with_existing_path_fails() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");

        std::fs::create_dir(&project_path).unwrap();

        let result = init(&project_path, &projects_dir);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::PathAlreadyExists(_)));
    }

    #[test]
    fn init_creates_initial_commit_so_branches_work() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir).unwrap();

        // Should be able to create a branch (requires at least one commit)
        let main_path = project_path.join("main");
        git::create_branch(&main_path, "test-branch").unwrap();
        assert!(git::branch_exists(&main_path, "test-branch").unwrap());
    }
}
