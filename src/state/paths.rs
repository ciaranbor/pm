use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};

pub use super::dirs::Dirs;

pub(crate) const PM_DIR_NAME: &str = ".pm";
const FEATURES_DIR_NAME: &str = "features";
pub(crate) const PROJECTS_DIR_NAME: &str = "projects";

pub(crate) const WORKFLOWS_DIR_NAME: &str = "workflows";

/// The XDG variable `name` of this process. Never under `cfg(test)`, so a
/// developer's own variables can't point a test at their real dirs.
fn xdg_env(name: &str) -> Option<std::ffi::OsString> {
    if cfg!(test) {
        return None;
    }
    std::env::var_os(name)
}

/// The user's home directory. Every global-tier path (the `~/.agents` store,
/// the pm config dir) derives from it. Under `cfg(test)` this is a per-binary
/// temp dir (see `testing::test_home`) so tests never read or write the
/// developer's real home.
pub fn home_dir() -> Result<PathBuf> {
    #[cfg(test)]
    {
        Ok(crate::testing::test_home().to_path_buf())
    }
    #[cfg(not(test))]
    {
        dirs::home_dir().ok_or(PmError::NoHomeDir)
    }
}

/// pm's global [`Dirs`] under `home`, by this process's XDG variables.
pub fn dirs_under(home: &Path) -> Dirs {
    Dirs::resolve(home, xdg_env)
}

/// pm's global [`Dirs`].
pub fn global_dirs() -> Result<Dirs> {
    Ok(dirs_under(&home_dir()?))
}

/// The pm config dir ([`Dirs::config`]): the project registry, global
/// `config.toml`, `notices.md`, and the global `workflows/` tier.
pub fn global_config_dir() -> Result<PathBuf> {
    Ok(global_dirs()?.config)
}

/// The pm state dir ([`Dirs::state`]).
pub fn global_state_dir() -> Result<PathBuf> {
    Ok(global_dirs()?.state)
}

/// The pm cache dir ([`Dirs::cache`]).
pub fn global_cache_dir() -> Result<PathBuf> {
    Ok(global_dirs()?.cache)
}

/// The pm runtime dir ([`Dirs::runtime`]).
pub fn global_runtime_dir() -> Result<PathBuf> {
    Ok(global_dirs()?.runtime)
}

/// The project registry: `<config dir>/projects/`.
pub fn global_projects_dir() -> Result<PathBuf> {
    Ok(global_config_dir()?.join(PROJECTS_DIR_NAME))
}

/// The global workflow tier: `<config dir>/workflows/`.
pub fn global_workflows_dir() -> Result<PathBuf> {
    Ok(global_config_dir()?.join(WORKFLOWS_DIR_NAME))
}

/// The global workflow tier under an explicit config dir; see
/// [`global_workflows_dir`].
pub fn global_workflows_dir_in(config_dir: &Path) -> PathBuf {
    config_dir.join(WORKFLOWS_DIR_NAME)
}

/// Returns the .pm/ directory for a given project root.
pub fn pm_dir(project_root: &Path) -> PathBuf {
    project_root.join(PM_DIR_NAME)
}

/// Returns the features state directory for a given project root.
pub fn features_dir(project_root: &Path) -> PathBuf {
    pm_dir(project_root).join(FEATURES_DIR_NAME)
}

/// Returns the messages directory for a given project root.
pub fn messages_dir(project_root: &Path) -> PathBuf {
    pm_dir(project_root).join("messages")
}

/// Returns the agents registry directory for a given project root.
pub fn agents_dir(project_root: &Path) -> PathBuf {
    pm_dir(project_root).join("agents")
}

/// Returns the docs (information store) directory for a given project root.
pub fn docs_dir(project_root: &Path) -> PathBuf {
    pm_dir(project_root).join("docs")
}

/// The information store's index of categories: `<project>/.pm/docs/categories.toml`.
pub fn doc_categories(project_root: &Path) -> PathBuf {
    docs_dir(project_root).join("categories.toml")
}

/// Feature summaries for the orchestrator: `<project>/.pm/summaries/`.
pub fn summaries_dir(project_root: &Path) -> PathBuf {
    pm_dir(project_root).join("summaries")
}

/// A feature's summary: `<project>/.pm/summaries/<feature>.md`.
pub fn summary_path(project_root: &Path, feature: &str) -> PathBuf {
    summaries_dir(project_root).join(format!("{feature}.md"))
}

/// The project's notes: `<project>/.pm/notes.md`.
pub fn notes_path(project_root: &Path) -> PathBuf {
    pm_dir(project_root).join("notes.md")
}

/// The project workflow tier: `<project>/.pm/workflows/`.
pub fn workflows_dir(project_root: &Path) -> PathBuf {
    pm_dir(project_root).join(WORKFLOWS_DIR_NAME)
}

/// One-shot migration markers, in the git-backed state dir so they travel
/// with the state they describe.
pub fn migrations_dir(project_root: &Path) -> PathBuf {
    pm_dir(project_root).join("migrations")
}

/// The marker recording that migration `name` has run for this project.
pub fn migration_marker(project_root: &Path, name: &str) -> PathBuf {
    migrations_dir(project_root).join(name)
}

/// Single source of truth for the main worktree directory name convention.
/// Returns `<project_root>/main`.
pub fn main_worktree(project_root: &Path) -> PathBuf {
    scope_worktree(project_root, crate::state::scope::MAIN)
}

/// A scope's worktree: `<project_root>/<scope>`, main's included.
pub fn scope_worktree(project_root: &Path, scope: &str) -> PathBuf {
    project_root.join(scope)
}

/// Walk up from `start` to find the project root: the nearest directory
/// containing `.pm/`, which must also be on this machine
/// ([`Presence`](crate::state::project::Presence)).
pub fn find_project_root(start: &Path) -> Result<PathBuf> {
    let mut current = start.to_path_buf();

    // Canonicalize to resolve symlinks and get absolute path
    if current.is_relative() {
        current = std::env::current_dir()?.join(current);
    }
    current = current.canonicalize()?;

    loop {
        if current.join(PM_DIR_NAME).is_dir() {
            if !crate::state::project::Presence::of(&current).is_here() {
                return Err(PmError::NotRestoredRoot(current));
            }
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

/// The environment variable naming the worktree an agent was spawned in.
/// Its hooks and `pm msg` calls resolve its scope from it rather than the
/// current directory: a harness runs them wherever the agent's shell last
/// `cd`'d to, which may be outside any worktree (an `--add-dir` root).
pub const AGENT_WORKTREE_ENV: &str = "PM_AGENT_WORKTREE";

/// The worktree of the agent this process runs for, from
/// [`AGENT_WORKTREE_ENV`]; `None` outside an agent.
pub fn agent_worktree() -> Option<PathBuf> {
    std::env::var_os(AGENT_WORKTREE_ENV)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// [`agent_worktree`], else the current directory.
pub fn agent_dir() -> Result<PathBuf> {
    match agent_worktree() {
        Some(dir) => Ok(dir),
        None => Ok(std::env::current_dir()?),
    }
}

/// The scope a command run from `cwd` acts on: the one `cwd` is in, else,
/// from outside every worktree, that of `agent_worktree` — an agent whose
/// shell left its worktree (main's works in `.pm/docs`).
pub fn command_scope(
    project_root: &Path,
    cwd: &Path,
    agent_worktree: Option<&Path>,
) -> Result<String> {
    match (resolve_scope_from(project_root, cwd), agent_worktree) {
        (Err(PmError::NotInWorktree), Some(dir)) => resolve_scope_from(project_root, dir),
        (resolved, _) => resolved,
    }
}

/// The project root and scope of the agent this process runs for, resolved
/// from [`agent_dir`].
pub fn agent_scope() -> Result<(PathBuf, String)> {
    let dir = agent_dir()?;
    let project_root = find_project_root(&dir)?;
    let scope = resolve_scope_from(&project_root, &dir)?;
    Ok((project_root, scope))
}

/// Resolve the current scope from a working directory: the feature name if
/// `cwd` is inside a known feature worktree, `"main"` if inside the main
/// worktree, otherwise [`PmError::NotInWorktree`].
pub fn resolve_scope_from(project_root: &Path, cwd: &Path) -> Result<String> {
    if let Some(feature) = detect_feature_from_cwd(project_root, cwd) {
        return Ok(feature);
    }
    if is_in_main_worktree(project_root, cwd) {
        return Ok("main".to_string());
    }
    Err(PmError::NotInWorktree)
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
        std::fs::create_dir(main_worktree(root)).unwrap();

        let found = find_project_root(root).unwrap();
        assert_eq!(found, root.canonicalize().unwrap());
    }

    #[test]
    fn find_project_root_from_worktree_subdirectory() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        // Simulate a worktree subdirectory: <root>/main/src/
        let deep = main_worktree(root).join("src");
        std::fs::create_dir_all(&deep).unwrap();

        let found = find_project_root(&deep).unwrap();
        assert_eq!(found, root.canonicalize().unwrap());
    }

    #[test]
    fn find_project_root_refuses_a_root_without_main() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("husk");
        std::fs::create_dir_all(root.join(".pm/messages")).unwrap();

        let err = find_project_root(&root.join(".pm/messages")).unwrap_err();
        assert!(
            matches!(&err, PmError::NotRestoredRoot(r) if *r == root.canonicalize().unwrap()),
            "{err}"
        );
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
    fn a_command_acts_on_the_cwds_scope_and_on_the_agents_only_from_outside_any() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        create_feature_state(root, "login");
        let login = root.join("login");
        let main = main_worktree(root);
        let docs = docs_dir(root);
        for d in [&login, &main, &docs] {
            std::fs::create_dir_all(d).unwrap();
        }

        assert_eq!(command_scope(root, &login, Some(&main)).unwrap(), "login");
        assert_eq!(command_scope(root, &docs, Some(&main)).unwrap(), "main");
        assert!(matches!(
            command_scope(root, &docs, None).unwrap_err(),
            PmError::NotInWorktree
        ));
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
        let main_dir = main_worktree(root).join("src");
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
        let main_dir = main_worktree(root).join("src");
        std::fs::create_dir_all(&main_dir).unwrap();

        assert!(is_in_main_worktree(root, &main_dir));
    }

    #[test]
    fn is_in_main_worktree_true_at_main_root() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let main_dir = main_worktree(root);
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
    fn resolve_scope_from_returns_feature_name_in_feature_worktree() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();
        create_feature_state(root, "login");
        let cwd = root.join("login").join("src");
        std::fs::create_dir_all(&cwd).unwrap();

        let scope = resolve_scope_from(root, &cwd).unwrap();
        assert_eq!(scope, "login");
    }

    #[test]
    fn resolve_scope_from_returns_main_in_main_worktree() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();
        let cwd = main_worktree(root).join("src");
        std::fs::create_dir_all(&cwd).unwrap();

        let scope = resolve_scope_from(root, &cwd).unwrap();
        assert_eq!(scope, "main");
    }

    #[test]
    fn resolve_scope_from_errors_outside_worktree() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = resolve_scope_from(root, root);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::NotInWorktree));
    }

    #[test]
    fn global_dirs_hang_off_the_test_home_under_test() {
        let home = home_dir().unwrap();
        assert!(home.starts_with(std::env::temp_dir()));
        assert!(global_config_dir().unwrap().starts_with(&home));
        assert!(global_state_dir().unwrap().starts_with(&home));
        assert!(global_runtime_dir().unwrap().starts_with(&home));
        assert!(global_projects_dir().unwrap().ends_with("pm/projects"));
        assert!(global_workflows_dir().unwrap().ends_with("pm/workflows"));
    }
}
