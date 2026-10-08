use std::path::{Path, PathBuf};

use crate::commands::agent_spawn::{agent_spawn, agent_spawn_in};
use crate::commands::hooks_install;
use crate::commands::skills::{self, GlobalStore};
use crate::error::{PmError, Result};
use crate::git;
use crate::hooks;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::{AgentsConfig, ProjectConfig, ProjectEntry, ProjectInfo};
use crate::tmux;

/// Initialize a new pm project at the given path.
///
/// Creates:
/// - `<path>/` — project root
/// - `<path>/main/` — main worktree with git init (or git clone if `git_url` provided)
/// - `<path>/.pm/` — project state directory
/// - `<path>/.pm/config.toml` — project config
/// - `<path>/.pm/features/` — empty features directory
/// - `<pm config dir>/projects/<name>.toml` — global registry entry
/// - `<name>/main` tmux session pointing at the main worktree
///
/// It also installs the global asset tier and the pm hooks.
///
/// Returns the project root, made absolute.
///
/// The project is named after its directory. A name the registry holds for
/// another project is refused.
///
/// The `tmux_server` parameter allows tests to use an isolated tmux server.
pub fn init(
    path: &Path,
    projects_dir: &Path,
    git_url: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<PathBuf> {
    init_in(
        path,
        None,
        projects_dir,
        &GlobalStore::resolve()?,
        git_url,
        tmux_server,
    )
}

/// [`init`] installing into an explicit global tier, under `name` when
/// given rather than the directory's.
pub fn init_in(
    path: &Path,
    name: Option<&str>,
    projects_dir: &Path,
    global: &GlobalStore,
    git_url: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<PathBuf> {
    // Not `exists`, which follows symlinks: a dangling one would pass, and
    // the cleanup below would then remove it.
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(PmError::PathAlreadyExists(path.to_path_buf()));
    }

    let name = match name {
        Some(name) => name,
        None => path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
            PmError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid project path",
            ))
        })?,
    }
    .to_string();

    // Resolve to an absolute path before touching the filesystem. Without
    // this a relative argument (`pm init exo-bench`) would be saved verbatim
    // in the registry, then resolved against each caller's CWD — silently
    // corrupting cross-project operations like messaging.
    //
    // We use `absolutize` (CWD-join) rather than `canonicalize` so the user's
    // chosen path prefix is preserved (canonicalize would resolve symlinks
    // like /var → /private/var on macOS, which is more invasive than needed
    // and breaks path comparisons in tests).
    let path_buf = crate::path_utils::absolutize(path)?;
    let path = path_buf.as_path();
    ProjectEntry::ensure_name_free(projects_dir, &name, path)?;

    // A failure removes what this call created, so a retry finds no path in
    // the way. `path` was absent, so its outermost missing ancestor and
    // everything under it are new. The registry is saved last, so a failure
    // leaves no entry to undo.
    let created = topmost_missing(path);
    let session_name = tmux::session_name(&name, "main");
    let built = populate(path, &name, global, git_url).and_then(|main_branch| {
        tmux::create_session(tmux_server, &session_name, &paths::main_worktree(path))?;
        let entry = ProjectEntry {
            root: crate::path_utils::to_portable(path),
            main_branch,
            repo_url: git_url.map(|u| u.to_string()),
            state_remote: None,
        };
        entry.save(projects_dir, &name).inspect_err(|_| {
            let _ = tmux::kill_session(tmux_server, &session_name);
        })
    });
    if let Err(e) = built {
        let _ = std::fs::remove_dir_all(&created);
        return Err(e);
    }

    Ok(path_buf)
}

/// The part of [`init_in`] that builds the project on disk, returning its
/// main branch.
fn populate(
    path: &Path,
    name: &str,
    global: &GlobalStore,
    git_url: Option<&str>,
) -> Result<String> {
    // Create project root
    std::fs::create_dir_all(path)?;

    // Init or clone git repo in main/
    let main_path = paths::main_worktree(path);
    let main_branch = if let Some(url) = git_url {
        git::clone_repo(url, &main_path)?;
        git::main_branch(&main_path)?
    } else {
        git::init_repo(&main_path)?;
        "main".to_string()
    };

    scaffold_state(path, name, global)?;

    Ok(main_branch)
}

/// Write the `.pm/` state of a new project named `name` at `path`, and
/// install what every project needs: its hooks, the global asset tier and
/// the pm hooks in each harness's settings. A project scaffolded here holds no
/// bundled copies, so it is born migrated.
pub(super) fn scaffold_state(path: &Path, name: &str, global: &GlobalStore) -> Result<()> {
    std::fs::create_dir_all(paths::features_dir(path))?;
    ProjectConfig {
        project: ProjectInfo {
            name: name.to_string(),
            max_features: None,
        },
        agents: AgentsConfig::default(),
        harness: Default::default(),
    }
    .save(&paths::pm_dir(path))?;

    hooks::bootstrap(path)?;
    super::docs::bootstrap(path)?;
    super::state_cmd::init(path)?;
    // See `commands::hooks_install` for why every harness gets them.
    hooks_install::install_in(&global.home, Some(path), false)?;
    skills::install_global_in(global)?;
    skills::write_migration_marker(path)?;
    super::vanilla_rename::write_marker(path)?;
    Ok(())
}

/// The outermost of `path` and its ancestors that does not exist.
fn topmost_missing(path: &Path) -> PathBuf {
    let mut top = path;
    while let Some(parent) = top.parent() {
        if parent.as_os_str().is_empty() || std::fs::symlink_metadata(parent).is_ok() {
            break;
        }
        top = parent;
    }
    top.to_path_buf()
}

/// The project root `pm init --git <url>` uses without a PATH: `./<repo
/// name>`, the directory `git clone <url>` would create.
pub fn default_path(git_url: &str) -> Result<PathBuf> {
    let trimmed = git_url.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/.git").unwrap_or(trimmed);
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or_default();
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty() || name == "." || name == ".." {
        return Err(PmError::UnnamedGitUrl(git_url.to_string()));
    }
    Ok(PathBuf::from(name))
}

/// Spawn the project's `main` agent in its main session: into the session's
/// first window, the shell it was created with, unless `main` already has
/// one of its own.
pub fn spawn_main(project_root: &Path, tmux_server: Option<&str>) -> Result<String> {
    let config = ProjectConfig::load(&paths::pm_dir(project_root))?;
    let registered = AgentRegistry::load(&paths::agents_dir(project_root), "main")?
        .get("main")
        .is_some();
    let (_, msg, _) = if registered {
        agent_spawn(project_root, "main", "main", None, None, tmux_server)?
    } else {
        let first = format!("{}:0", tmux::session_name(&config.project.name, "main"));
        agent_spawn_in(project_root, "main", "main", &first, tmux_server)?
    };
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn default_path_is_the_repo_name_git_clone_would_use() {
        for (url, name) in [
            ("git@github.com:me/foo.git", "foo"),
            ("https://github.com/org/myapp.git", "myapp"),
            ("https://github.com/org/myapp/", "myapp"),
            ("ssh://host:2222/srv/repo.git", "repo"),
            ("host:repo.git", "repo"),
            ("/srv/git/proj/.git", "proj"),
        ] {
            assert_eq!(default_path(url).unwrap(), PathBuf::from(name), "{url}");
        }
    }

    #[test]
    fn default_path_refuses_a_url_with_no_repo_name() {
        for url in ["", "/", "https://host/..", ".git"] {
            assert!(
                matches!(default_path(url), Err(PmError::UnnamedGitUrl(_))),
                "{url}"
            );
        }
    }

    #[test]
    fn init_creates_main_directory() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        assert!(paths::main_worktree(&project_path).exists());
        assert!(paths::main_worktree(&project_path).is_dir());
    }

    #[test]
    fn init_creates_git_repo_in_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        assert!(paths::main_worktree(&project_path).join(".git").exists());
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
    fn init_installs_no_bundled_assets_in_the_project() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");

        init(&project_path, &projects_dir, None, server.name()).unwrap();

        let main = paths::main_worktree(&project_path);
        assert!(!main.join(".agents").exists());
        assert!(!main.join(".claude/agents").exists());
        assert!(!main.join(".claude/skills").exists());
        // Hooks are user-level; a fresh project gets no settings file.
        assert!(!main.join(".claude/settings.json").exists());
        assert!(!paths::workflows_dir(&project_path).exists());
        assert!(skills::is_migrated(&project_path));
    }

    #[test]
    fn init_installs_the_global_tier_and_every_harness_hook_into_its_home() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let global = GlobalStore::at(&home);
        let project_path = dir.path().join(server.scope("myapp"));

        init_in(
            &project_path,
            None,
            &dir.path().join("registry"),
            &global,
            None,
            server.name(),
        )
        .unwrap();

        assert_eq!(
            skills::global_store_missing_in(&global),
            Vec::<String>::new()
        );
        for &harness in crate::harness::Harness::SUPPORTED {
            assert!(
                hooks_install::is_installed_in(harness, &home).unwrap(),
                "{harness}"
            );
        }
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
    fn init_refuses_a_name_the_registry_holds_for_another_project() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let projects_dir = dir.path().join("registry");
        let first = dir.path().join("a").join(&name);
        init(&first, &projects_dir, None, server.name()).unwrap();

        let second = dir.path().join("b").join(&name);
        let err = init(&second, &projects_dir, None, server.name()).unwrap_err();

        assert!(matches!(err, PmError::ProjectNameTaken { .. }), "{err}");
        assert!(!second.exists());
        let entry = ProjectEntry::load(&projects_dir, &name).unwrap();
        assert_eq!(entry.root_path(), first);
    }

    #[test]
    fn a_failed_clone_leaves_no_directory_in_the_way_of_a_retry() {
        let _cwd = crate::testing::CWD_LOCK
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");

        let missing = dir.path().join("missing.git");
        let result = init(
            &project_path,
            &projects_dir,
            Some(&missing.to_string_lossy()),
            None,
        );

        assert!(matches!(result, Err(PmError::Git(_))), "{result:?}");
        assert!(!project_path.exists());
        assert!(ProjectEntry::list(&projects_dir).unwrap().is_empty());
    }

    #[test]
    fn a_failure_before_registration_removes_only_what_init_created() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::write(&home, "not a directory").unwrap();
        let created = dir.path().join("new");
        let project_path = created.join("nested").join("myapp");
        let projects_dir = dir.path().join("registry");

        let result = init_in(
            &project_path,
            None,
            &projects_dir,
            &GlobalStore::at(&home),
            None,
            None,
        );

        assert!(result.is_err());
        assert!(!created.exists());
        assert!(dir.path().exists());
        assert!(home.is_file());
        assert!(ProjectEntry::list(&projects_dir).unwrap().is_empty());
    }

    #[test]
    fn a_failed_session_leaves_no_registry_entry_or_directory() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        let session = tmux::session_name(&name, "main");
        tmux::create_session(server.name(), &session, dir.path()).unwrap();

        let result = init(&project_path, &projects_dir, None, server.name());

        assert!(matches!(result, Err(PmError::Tmux(_))), "{result:?}");
        assert!(!project_path.exists());
        assert!(ProjectEntry::list(&projects_dir).unwrap().is_empty());
    }

    #[test]
    fn a_dangling_symlink_at_the_path_is_refused_and_kept() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        std::os::unix::fs::symlink(dir.path().join("gone"), &project_path).unwrap();

        let result = init(&project_path, &projects_dir, None, None);

        assert!(
            matches!(result, Err(PmError::PathAlreadyExists(_))),
            "{result:?}"
        );
        assert!(std::fs::symlink_metadata(&project_path).is_ok());
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
        let main_path = paths::main_worktree(&project_path);
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

        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());
    }

    #[test]
    fn main_is_spawned_into_the_main_sessions_first_window_once() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init(&project_path, &projects_dir, None, server.name()).unwrap();
        let session = tmux::session_name(&name, "main");

        spawn_main(&project_path, server.name()).unwrap();
        let again = spawn_main(&project_path, server.name()).unwrap();

        assert!(again.contains("already active"), "{again}");
        assert_eq!(
            tmux::find_window(server.name(), &session, "main").unwrap(),
            Some(format!("{session}:0"))
        );
        assert_eq!(tmux::list_windows(server.name(), &session).unwrap(), 1);
        let registry = AgentRegistry::load(&paths::agents_dir(&project_path), "main").unwrap();
        assert!(registry.get("main").unwrap().active);
    }

    #[test]
    fn init_with_git_url_clones_repo() {
        // Read side of CWD_LOCK (serialises against the CWD mutator) — see `testing::CWD_LOCK`.
        let _cwd = crate::testing::CWD_LOCK
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        assert!(paths::main_worktree(&project_path).join(".git").exists());
        // .pm/ structure should exist
        assert!(project_path.join(".pm").join("config.toml").exists());
        assert!(project_path.join(".pm").join("features").exists());
        // tmux session should exist
        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());
    }

    #[test]
    fn init_with_git_url_cloned_repo_has_remote() {
        // Read side of CWD_LOCK (serialises against the CWD mutator) — see `testing::CWD_LOCK`.
        let _cwd = crate::testing::CWD_LOCK
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let main_path = paths::main_worktree(&project_path);
        let remotes = crate::git::list_remotes(&main_path).unwrap();
        assert!(remotes.contains("origin"));
    }

    #[test]
    fn init_with_relative_path_stores_absolute_root_in_registry() {
        // Regression test for the bug where `pm init exo-bench` (relative
        // path) was saved as `root = "exo-bench"` in the registry, breaking
        // cross-project messaging because the path resolved against the
        // sender's CWD.
        //
        // CWD is process-global and `cargo test` runs tests in parallel, so
        // mutating it here can race the `git clone` tests (clone reads CWD at
        // startup and fails if CWD isn't a work tree). We take the WRITE side
        // of the shared `CWD_LOCK`; the clone tests take the READ side. This
        // serialises this mutator against those readers without flattening
        // parallelism. If you add another CWD-mutating test, take this write
        // lock too.
        let _guard = crate::testing::CWD_LOCK
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("relapp");
        let projects_dir = dir.path().join("registry");

        // Restores CWD however the test ends, so no other test runs in it.
        struct RestoreCwd(std::path::PathBuf);
        impl Drop for RestoreCwd {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let restore = RestoreCwd(std::env::current_dir().unwrap());
        std::env::set_current_dir(dir.path()).unwrap();

        // Pass just the relative name — this is what reproduced the bug
        let relative_path = std::path::PathBuf::from(&name);
        let result = init(&relative_path, &projects_dir, None, server.name());
        drop(restore);
        result.unwrap();

        let entry = ProjectEntry::load(&projects_dir, &name).unwrap();
        // root must be portable: absolute ('/…') or tilde ('~/…'), never bare
        assert!(
            crate::path_utils::is_portable(&entry.root),
            "registry root should be portable, got: {:?}",
            entry.root
        );
        assert_ne!(entry.root, name, "bug: registry stored relative path");
    }

    #[test]
    fn init_with_git_url_detects_default_branch() {
        // Read side of CWD_LOCK (serialises against the CWD mutator) — see `testing::CWD_LOCK`.
        let _cwd = crate::testing::CWD_LOCK
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                bare_path.to_string_lossy().as_ref(),
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
                staging.to_string_lossy().as_ref(),
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
