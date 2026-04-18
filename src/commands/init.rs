use std::path::Path;

use crate::commands::hooks_install;
use crate::commands::skills;
use crate::error::{PmError, Result};
use crate::git;
use crate::hooks;
use crate::state::paths;
use crate::state::project::{
    AgentsConfig, GithubConfig, ProjectConfig, ProjectEntry, ProjectInfo, SetupConfig,
};
use crate::tmux;

/// Initialize a new pm project at the given path.
///
/// Creates:
/// - `<path>/` — project root
/// - `<path>/main/` — main worktree with git init (or git clone if `git_url` provided)
/// - `<path>/.pm/` — project state directory
/// - `<path>/.pm/config.toml` — project config
/// - `<path>/.pm/features/` — empty features directory
/// - `~/.config/pm/projects/<name>.toml` — global registry entry
/// - `<name>/main` tmux session pointing at the main worktree
///
/// The `tmux_server` parameter allows tests to use an isolated tmux server.
pub fn init(
    path: &Path,
    projects_dir: &Path,
    git_url: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<()> {
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

    // Init or clone git repo in main/
    let main_path = path.join("main");
    let main_branch = if let Some(url) = git_url {
        git::clone_repo(url, &main_path)?;
        // Detect default branch from the cloned remote
        git::default_branch(&main_path).unwrap_or_else(|_| "main".to_string())
    } else {
        git::init_repo(&main_path)?;
        "main".to_string()
    };

    // Create .pm/ structure
    let pm_dir = paths::pm_dir(path);
    let features_dir = paths::features_dir(path);
    std::fs::create_dir_all(&features_dir)?;

    // Write project config
    let config = ProjectConfig {
        project: ProjectInfo { name: name.clone() },
        setup: SetupConfig::default(),
        github: GithubConfig::default(),
        agents: {
            let mut permissions = std::collections::BTreeMap::new();
            permissions.insert("implementer".to_string(), "acceptEdits".to_string());
            AgentsConfig {
                default: "implementer".to_string(),
                permissions,
            }
        },
    };
    config.save(&pm_dir)?;

    // Bootstrap default hook scripts
    hooks::bootstrap(path)?;

    // Bootstrap the information store (.pm/docs/) and state repo (.pm/)
    super::docs::bootstrap(path)?;
    super::state_cmd::init(path)?;

    // Install the pm Stop hook into main/.claude/settings.json so every
    // agent spawned under this project runs as a never-idle message
    // processor (see `commands::hooks_install`).
    hooks_install::install(path)?;

    // Install bundled skills and agent definitions into main/.claude/
    // so the project is immediately ready for agent workflows.
    skills::skills_install_project(path, None)?;
    skills::agents_install_project(path, None)?;

    // Register in global registry
    let entry = ProjectEntry {
        root: crate::path_utils::to_portable(path),
        main_branch,
    };
    entry.save(projects_dir, &name)?;

    // Create main tmux session
    let session_name = format!("{name}/main");
    tmux::create_session(tmux_server, &session_name, &main_path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn init_creates_main_directory() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        assert!(project_path.join("main").exists());
        assert!(project_path.join("main").is_dir());
    }

    #[test]
    fn init_creates_git_repo_in_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        assert!(project_path.join("main").join(".git").exists());
    }

    #[test]
    fn init_creates_pm_directory_with_config_and_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        assert!(project_path.join(".pm").exists());
        assert!(project_path.join(".pm").join("config.toml").exists());
        assert!(project_path.join(".pm").join("features").exists());
    }

    #[test]
    fn init_bootstraps_hook_scripts() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        assert!(project_path.join(hooks::POST_CREATE_PATH).is_file());
        assert!(project_path.join(hooks::POST_MERGE_PATH).is_file());
    }

    #[test]
    fn init_installs_skills_and_agents() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Skills should be installed into main/.claude/skills/
        let skill_path = project_path
            .join("main")
            .join(".claude")
            .join("skills")
            .join("pm")
            .join("SKILL.md");
        assert!(skill_path.exists(), "pm skill should be installed");

        // Agent definitions should be installed into main/.claude/agents/
        let agent_path = project_path
            .join("main")
            .join(".claude")
            .join("agents")
            .join("reviewer.md");
        assert!(agent_path.exists(), "reviewer agent should be installed");
    }

    #[test]
    fn init_bootstraps_docs() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        let docs_dir = project_path.join(".pm").join("docs");
        assert!(docs_dir.join("categories.toml").exists());
        assert!(docs_dir.join("todo.md").exists());
        assert!(docs_dir.join("issues.md").exists());
        assert!(docs_dir.join("ideas.md").exists());
        // Docs tracked by parent .pm/ state repo, not a separate git repo
        assert!(!docs_dir.join(".git").exists());
        assert!(project_path.join(".pm").join(".git").exists());
    }

    #[test]
    fn init_writes_correct_project_name_in_config() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        let pm_dir = project_path.join(".pm");
        let config = ProjectConfig::load(&pm_dir).unwrap();
        assert_eq!(config.project.name, name);
    }

    #[test]
    fn init_registers_project_in_global_registry() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        let entry = ProjectEntry::load(&projects_dir, &name).unwrap();
        assert_eq!(entry.root, crate::path_utils::to_portable(&project_path));
        assert_eq!(entry.main_branch, "main");
    }

    #[test]
    fn init_with_existing_path_fails() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");

        std::fs::create_dir(&project_path).unwrap();

        let result = init(&project_path, &projects_dir, None, None);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::PathAlreadyExists(_)));
    }

    #[test]
    fn init_creates_initial_commit_so_branches_work() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Should be able to create a branch (requires at least one commit)
        let main_path = project_path.join("main");
        git::create_branch(&main_path, "test-branch").unwrap();
        assert!(git::branch_exists(&main_path, "test-branch").unwrap());
    }

    #[test]
    fn init_creates_main_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &format!("{name}/main")).unwrap());
    }

    #[test]
    fn init_with_git_url_clones_repo() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();

        // Create a bare repo to act as the remote
        let bare_path = dir.path().join("remote.git");
        crate::git::init_bare(&bare_path).unwrap();

        // Push an initial commit to it so it has content
        let staging = dir.path().join("staging");
        crate::git::init_repo(&staging).unwrap();
        crate::git::add_remote(&staging, "origin", &bare_path.to_string_lossy()).unwrap();
        crate::git::push(&staging, "origin", "main").unwrap();

        let name = server.scope("cloned");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(
            &project_path,
            &projects_dir,
            Some(&bare_path.to_string_lossy()),
            server.name(),
        )
        .unwrap();

        // main/ should exist and be a git repo
        assert!(project_path.join("main").join(".git").exists());
        // .pm/ structure should exist
        assert!(project_path.join(".pm").join("config.toml").exists());
        assert!(project_path.join(".pm").join("features").exists());
        // tmux session should exist
        assert!(tmux::has_session(server.name(), &format!("{name}/main")).unwrap());
    }

    #[test]
    fn init_with_git_url_cloned_repo_has_remote() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();

        let bare_path = dir.path().join("remote.git");
        crate::git::init_bare(&bare_path).unwrap();

        let staging = dir.path().join("staging");
        crate::git::init_repo(&staging).unwrap();
        crate::git::add_remote(&staging, "origin", &bare_path.to_string_lossy()).unwrap();
        crate::git::push(&staging, "origin", "main").unwrap();

        let name = server.scope("cloned");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(
            &project_path,
            &projects_dir,
            Some(&bare_path.to_string_lossy()),
            server.name(),
        )
        .unwrap();

        // The cloned repo should have an origin remote
        let main_path = project_path.join("main");
        let remotes = crate::git::list_remotes(&main_path).unwrap();
        assert!(remotes.contains("origin"));
    }

    #[test]
    fn init_with_git_url_detects_default_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();

        // Create a bare repo with "master" as default branch
        let bare_path = dir.path().join("remote.git");
        std::fs::create_dir_all(&bare_path).unwrap();
        std::process::Command::new("git")
            .args([
                "init",
                "--bare",
                "--initial-branch=master",
                &bare_path.to_string_lossy().to_string(),
            ])
            .output()
            .unwrap();

        // Push content so the remote has a HEAD
        let staging = dir.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::process::Command::new("git")
            .args([
                "init",
                "--initial-branch=master",
                &staging.to_string_lossy().to_string(),
            ])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-C",
                &staging.to_string_lossy(),
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .output()
            .unwrap();
        crate::git::add_remote(&staging, "origin", &bare_path.to_string_lossy()).unwrap();
        crate::git::push_branch(&staging, "master").unwrap();

        let name = server.scope("masterproj");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(
            &project_path,
            &projects_dir,
            Some(&bare_path.to_string_lossy()),
            server.name(),
        )
        .unwrap();

        // Registry should record "master" as the main branch
        let entry = ProjectEntry::load(&projects_dir, &name).unwrap();
        assert_eq!(entry.main_branch, "master");
    }
}
