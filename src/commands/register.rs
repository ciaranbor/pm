use std::path::Path;

use crate::error::{PmError, Result};
use crate::hooks;
use crate::state::paths;
use crate::state::project::{GithubConfig, ProjectConfig, ProjectEntry, ProjectInfo, SetupConfig};
use crate::tmux;

/// Register an existing git repo as a pm project.
///
/// Creates a wrapper directory (`<name>-pm/`) and either symlinks or moves
/// the original repo as `main/` inside it. Sets up the `.pm/` state directory
/// and creates a `<project>/main` tmux session.
///
/// The `tmux_server` parameter allows tests to use an isolated tmux server.
pub fn register(
    repo_path: &Path,
    name: Option<&str>,
    projects_dir: &Path,
    move_repo: bool,
    tmux_server: Option<&str>,
    claude_base: Option<&Path>,
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

    // Check if this repo is already registered under a different name
    for (existing_name, entry) in ProjectEntry::list(projects_dir)? {
        let existing_main = Path::new(&entry.root).join("main");
        if let Ok(existing_canonical) = existing_main.canonicalize()
            && existing_canonical == repo_path
        {
            return Err(PmError::RepoAlreadyRegistered(existing_name));
        }
    }

    let parent = repo_path.parent().ok_or_else(|| {
        PmError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "repo path has no parent directory",
        ))
    })?;

    let wrapper_dir = if move_repo {
        // Move mode: wrapper takes the project name directly (no -pm suffix).
        // The original repo path is vacated by the move.
        let wrapper = parent.join(&project_name);

        // Temporarily rename the repo so we can create the wrapper at the target path
        let tmp_name = parent.join(format!(".{project_name}-pm-tmp"));
        if tmp_name.exists() {
            return Err(PmError::PathAlreadyExists(tmp_name));
        }
        std::fs::rename(&repo_path, &tmp_name)?;
        std::fs::create_dir_all(&wrapper)?;
        std::fs::rename(&tmp_name, wrapper.join("main"))?;
        wrapper
    } else {
        // Symlink mode: wrapper gets -pm suffix to avoid collision with the original repo
        let wrapper = parent.join(format!("{project_name}-pm"));
        if wrapper.exists() {
            return Err(PmError::PathAlreadyExists(wrapper));
        }
        std::fs::create_dir_all(&wrapper)?;

        let main_path = wrapper.join("main");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&repo_path, &main_path)?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&repo_path, &main_path)?;
        wrapper
    };

    // Migrate Claude Code sessions from original repo path to new main path
    let new_main = wrapper_dir.join("main");
    match super::claude_migrate::migrate_sessions(&repo_path, &new_main, claude_base) {
        Ok(msgs) => {
            for msg in msgs {
                eprintln!("{msg}");
            }
        }
        Err(e) => eprintln!("Warning: Claude session migration failed: {e}"),
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
        agents: Default::default(),
    };
    config.save(&pm_dir)?;

    // Bootstrap default hook scripts
    hooks::bootstrap(&wrapper_dir)?;

    // Register in global registry
    let entry = ProjectEntry {
        root: wrapper_dir.to_string_lossy().to_string(),
        main_branch: "main".to_string(),
    };
    entry.save(projects_dir, &project_name)?;

    // Create main tmux session
    let session_name = format!("{project_name}/main");
    let main_path = wrapper_dir.join("main");
    tmux::create_session(tmux_server, &session_name, &main_path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn create_git_repo(path: &Path) {
        git::init_repo(path).unwrap();
    }

    #[test]
    fn register_creates_wrapper_directory() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let repo_path = dir.path().join(&name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false, server.name(), None).unwrap();

        let wrapper = dir.path().join(format!("{name}-pm"));
        assert!(wrapper.exists());
        assert!(wrapper.is_dir());
    }

    #[test]
    fn register_symlink_creates_symlink_to_original_repo() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let repo_path = dir.path().join(&name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false, server.name(), None).unwrap();

        let symlink = dir.path().join(format!("{name}-pm")).join("main");
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
        let server = TestServer::new();
        let name = server.scope("myapp");
        let repo_path = dir.path().join(&name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, true, server.name(), None).unwrap();

        // With --move, wrapper uses the project name directly (no -pm suffix)
        let wrapper = dir.path().join(&name);
        let main_path = wrapper.join("main");
        assert!(main_path.exists());
        assert!(main_path.join(".git").exists());
        assert!(!main_path.is_symlink());
        // Wrapper dir now has .pm/
        assert!(wrapper.join(".pm").exists());
    }

    #[test]
    fn register_creates_pm_structure() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let repo_path = dir.path().join(&name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false, server.name(), None).unwrap();

        let wrapper = dir.path().join(format!("{name}-pm"));
        assert!(wrapper.join(".pm").exists());
        assert!(wrapper.join(".pm").join("config.toml").exists());
        assert!(wrapper.join(".pm").join("features").exists());
        assert!(wrapper.join(hooks::POST_CREATE_PATH).is_file());
        assert!(wrapper.join(hooks::POST_MERGE_PATH).is_file());
    }

    #[test]
    fn register_writes_correct_root_in_registry() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let repo_path = dir.path().join(&name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false, server.name(), None).unwrap();

        let entry = ProjectEntry::load(&projects_dir, &name).unwrap();
        // Compare canonicalized paths to handle /var vs /private/var on macOS
        let entry_root = Path::new(&entry.root).canonicalize().unwrap();
        let expected = dir
            .path()
            .join(format!("{name}-pm"))
            .canonicalize()
            .unwrap();
        assert_eq!(entry_root, expected);
    }

    #[test]
    fn register_with_custom_name() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let repo_name = server.scope("myapp");
        let custom_name = server.scope("custom");
        let repo_path = dir.path().join(&repo_name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(
            &repo_path,
            Some(&custom_name),
            &projects_dir,
            false,
            server.name(),
            None,
        )
        .unwrap();

        let wrapper = dir.path().join(format!("{custom_name}-pm"));
        assert!(wrapper.exists());
        assert!(ProjectEntry::load(&projects_dir, &custom_name).is_ok());
    }

    #[test]
    fn register_non_git_repo_fails() {
        let dir = tempdir().unwrap();
        let not_a_repo = dir.path().join("not-a-repo");
        std::fs::create_dir(&not_a_repo).unwrap();
        let projects_dir = dir.path().join("registry");

        let result = register(&not_a_repo, None, &projects_dir, false, None, None);
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

        let result = register(&repo_path, None, &projects_dir, false, None, None);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::PathAlreadyExists(_)));
    }

    #[test]
    fn register_same_repo_twice_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let repo_path = dir.path().join(&name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false, server.name(), None).unwrap();

        // Second register detects duplicate repo in the registry
        let result = register(&repo_path, None, &projects_dir, false, server.name(), None);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PmError::RepoAlreadyRegistered(_)
        ));
    }

    #[test]
    fn register_same_repo_different_name_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let repo_path = dir.path().join(&name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false, server.name(), None).unwrap();

        // Try to register the same repo under a different name
        let other_name = server.scope("other-name");
        let result = register(
            &repo_path,
            Some(&other_name),
            &projects_dir,
            false,
            server.name(),
            None,
        );
        assert!(result.is_err());
        match result.unwrap_err() {
            PmError::RepoAlreadyRegistered(registered_name) => assert_eq!(registered_name, name),
            other => panic!("expected RepoAlreadyRegistered, got: {other}"),
        }
    }

    #[test]
    fn register_worktree_checkout_where_git_is_a_file() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let wt_proj_name = server.scope("wt-proj");
        let main_repo = dir.path().join("main-repo");
        create_git_repo(&main_repo);

        // Create a branch and a worktree — its .git is a file, not a directory
        git::create_branch(&main_repo, "wt-branch").unwrap();
        let wt_path = dir.path().join("wt-checkout");
        git::add_worktree(&main_repo, &wt_path, "wt-branch").unwrap();

        // Sanity: .git should be a file in a worktree
        assert!(wt_path.join(".git").is_file());

        let projects_dir = dir.path().join("registry");

        register(
            &wt_path,
            Some(&wt_proj_name),
            &projects_dir,
            false,
            server.name(),
            None,
        )
        .unwrap();

        let wrapper = dir.path().join(format!("{wt_proj_name}-pm"));
        assert!(wrapper.exists());
        assert!(wrapper.join(".pm").join("config.toml").exists());
        assert!(tmux::has_session(server.name(), &format!("{wt_proj_name}/main")).unwrap());
    }

    #[test]
    fn register_creates_main_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let repo_path = dir.path().join(&name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        register(&repo_path, None, &projects_dir, false, server.name(), None).unwrap();

        assert!(tmux::has_session(server.name(), &format!("{name}/main")).unwrap());
    }

    #[test]
    fn register_symlink_migrates_claude_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let repo_path = dir.path().join(&name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        // Set up fake Claude session data keyed to the original repo path
        let claude_base = dir.path().join("claude");
        let repo_canonical = repo_path.canonicalize().unwrap();
        let old_key = repo_canonical.to_string_lossy().replace('/', "-");
        let old_session_dir = claude_base.join("projects").join(&old_key);
        std::fs::create_dir_all(&old_session_dir).unwrap();
        std::fs::write(
            old_session_dir.join("session.jsonl"),
            format!("{{\"cwd\":\"{}\"}}\n", repo_canonical.display()),
        )
        .unwrap();

        register(
            &repo_path,
            None,
            &projects_dir,
            false,
            server.name(),
            Some(claude_base.as_path()),
        )
        .unwrap();

        // register canonicalizes repo_path, so wrapper_dir is built from canonical parent
        let canonical_parent = repo_canonical.parent().unwrap();
        let new_main = canonical_parent.join(format!("{name}-pm")).join("main");
        let new_key = new_main.to_string_lossy().replace('/', "-");
        let new_session_dir = claude_base.join("projects").join(&new_key);
        assert!(new_session_dir.exists());
        let content = std::fs::read_to_string(new_session_dir.join("session.jsonl")).unwrap();
        assert!(content.contains(&new_main.to_string_lossy().to_string()));
    }

    #[test]
    fn register_move_migrates_claude_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let repo_path = dir.path().join(&name);
        create_git_repo(&repo_path);
        let projects_dir = dir.path().join("registry");

        // Set up fake Claude session data keyed to the original repo path
        let claude_base = dir.path().join("claude");
        let repo_canonical = repo_path.canonicalize().unwrap();
        let old_key = repo_canonical.to_string_lossy().replace('/', "-");
        let old_session_dir = claude_base.join("projects").join(&old_key);
        std::fs::create_dir_all(&old_session_dir).unwrap();
        std::fs::write(
            old_session_dir.join("session.jsonl"),
            format!("{{\"cwd\":\"{}\"}}\n", repo_canonical.display()),
        )
        .unwrap();

        register(
            &repo_path,
            None,
            &projects_dir,
            true,
            server.name(),
            Some(claude_base.as_path()),
        )
        .unwrap();

        // With --move, wrapper is at canonical parent + scoped name, main inside it
        let canonical_parent = repo_canonical.parent().unwrap();
        let new_main = canonical_parent.join(&name).join("main");
        let new_key = new_main.to_string_lossy().replace('/', "-");
        let new_session_dir = claude_base.join("projects").join(&new_key);
        assert!(new_session_dir.exists());
        let content = std::fs::read_to_string(new_session_dir.join("session.jsonl")).unwrap();
        assert!(content.contains(&new_main.to_string_lossy().to_string()));
    }
}
