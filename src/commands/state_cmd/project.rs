//! The project state repo, `<project>/.pm/`.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;
use crate::state::paths;

use super::init::{InitConfig, init_repo_managed};
use super::remote::prompt_remote_setup_common;
use super::repo::{RepoContext, pull_repo, push_repo, require_repo, status_repo};

fn project_ctx(pm_dir: &Path) -> RepoContext<'_> {
    RepoContext {
        dir: pm_dir,
        label: "state",
        init_hint: "pm state init",
        remote_hint: "pm state remote <url>",
    }
}

/// Initialise a git repo in `.pm/` for state backup/sync.
///
/// Commits the current state. Idempotent. When called non-interactively
/// (e.g. from `pm init` or `pm upgrade`), skips the remote setup prompt.
pub fn init(project_root: &Path) -> Result<String> {
    init_inner(project_root, false, None)
}

/// Returns `true` if [`init`] would create a new `.pm/` state repo.
pub fn would_init(project_root: &Path) -> bool {
    let pm_dir = paths::pm_dir(project_root);
    pm_dir.exists() && !pm_dir.join(".git").exists()
}

/// Initialise with an explicit remote URL (combines init + remote + pull).
pub fn init_with_remote(project_root: &Path, remote_url: Option<&str>) -> Result<String> {
    match remote_url {
        Some(url) => init_inner(project_root, false, Some(url)),
        None => init_inner(project_root, true, None),
    }
}

fn init_inner(project_root: &Path, interactive: bool, remote_url: Option<&str>) -> Result<String> {
    let pm_dir = paths::pm_dir(project_root);
    let cfg = InitConfig {
        dir: &pm_dir,
        label: "state",
        dir_missing_error: ".pm/ directory does not exist — is this a pm project?",
        init_commit_msg: "init state repo",
        init_success_msg: "Initialised state repo in .pm/".to_string(),
        already_init_msg: "State repo already initialised",
        pull_hint: "pm state pull",
        reset_when_diverged: false,
        pre_init: None,
        post_remote: Some(Box::new(|| persist_state_remote_to_registry(project_root))),
        prompt_remote: Some(Box::new(|dir: &Path| {
            prompt_remote_setup(project_root, dir)
        })),
    };
    init_repo_managed(cfg, interactive, remote_url).map(|(result, _)| result)
}

/// Set the remote URL for the state repo.
///
/// If `url` is `Some`, sets the remote directly. If `None`, runs the
/// interactive prompt (create GitHub repo / use existing URL / skip).
///
/// Also persists the URL to the global registry entry's `state_remote` field.
pub fn remote(project_root: &Path, url: Option<&str>) -> Result<String> {
    let pm_dir = paths::pm_dir(project_root);
    let ctx = project_ctx(&pm_dir);
    require_repo(&ctx)?;

    if git::has_remote(ctx.dir, "origin")? {
        return Err(PmError::Git(format!(
            "remote 'origin' already exists (remove it with `git -C {} remote remove origin` to reset)",
            ctx.dir.display()
        )));
    }

    let result = match url {
        Some(url) => {
            git::add_remote(ctx.dir, "origin", url)?;
            Ok(format!("Set {} remote to {url}", ctx.label))
        }
        None => match prompt_remote_setup(project_root, ctx.dir)? {
            Some(msg) => Ok(msg),
            None => Ok("Skipped remote setup".to_string()),
        },
    };

    // Persist state_remote to the global registry
    if result.is_ok()
        && let Err(e) = persist_state_remote_to_registry(project_root)
    {
        eprintln!("warning: failed to persist state_remote to registry: {e}");
    }

    result
}

/// Auto-commit and push the state repo.
pub fn push(project_root: &Path) -> Result<String> {
    let pm_dir = paths::pm_dir(project_root);
    push_repo(&project_ctx(&pm_dir))
}

/// Pull state from the remote.
pub fn pull(project_root: &Path) -> Result<String> {
    let pm_dir = paths::pm_dir(project_root);
    pull_repo(&project_ctx(&pm_dir))
}

/// Show git status of the state repo.
pub fn status(project_root: &Path) -> Result<String> {
    let pm_dir = paths::pm_dir(project_root);
    status_repo(&project_ctx(&pm_dir))
}

/// Project-level remote setup prompt (derives repo name from project).
fn prompt_remote_setup(project_root: &Path, pm_dir: &Path) -> Result<Option<String>> {
    let project_name = derive_project_name(project_root);
    let repo_name = format!("{project_name}-pm-state");
    prompt_remote_setup_common(pm_dir, "project state", &repo_name)
}

/// Derive a project name from the project root for repo naming.
fn derive_project_name(project_root: &Path) -> String {
    // Try to read the project config for the canonical name
    let pm_dir = paths::pm_dir(project_root);
    if let Ok(config) = crate::state::project::ProjectConfig::load(&pm_dir) {
        return config.project.name;
    }
    // Fallback: use the directory name
    project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string()
}

/// Persist the .pm/ state repo's remote URL to the global registry entry.
fn persist_state_remote_to_registry(project_root: &Path) -> Result<()> {
    let pm_dir = paths::pm_dir(project_root);
    let url = git::remote_url(&pm_dir, "origin")?;
    if url.is_none() {
        return Ok(());
    }

    let name = derive_project_name(project_root);
    let projects_dir = paths::global_projects_dir()?;
    if let Ok(mut entry) = crate::state::project::ProjectEntry::load(&projects_dir, &name) {
        entry.state_remote = url;
        entry.save(&projects_dir, &name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::state_cmd::test_support::*;
    use tempfile::tempdir;

    fn setup_project(dir: &std::path::Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm").join("features")).unwrap();
        std::fs::create_dir_all(paths::main_worktree(&root)).unwrap();
        root
    }

    #[test]
    fn init_creates_git_repo_in_pm_dir() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let msg = init(&root).unwrap();
        assert!(msg.contains("Initialised"));
        assert!(root.join(".pm").join(".git").exists());
    }

    #[test]
    fn init_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        init(&root).unwrap();
        let msg = init(&root).unwrap();
        assert!(msg.contains("already initialised"));
    }

    #[test]
    fn init_errors_without_pm_dir() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // No .pm/ directory

        let result = init(&root);
        assert!(result.is_err());
    }

    #[test]
    fn status_shows_clean_after_init() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        init(&root).unwrap();
        let msg = status(&root).unwrap();
        assert!(msg.contains("clean"));
    }

    #[test]
    fn status_shows_changes_after_modification() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        init(&root).unwrap();

        // Create a new file in .pm/
        std::fs::write(root.join(".pm").join("features").join("test.toml"), "x").unwrap();

        let msg = status(&root).unwrap();
        assert!(!msg.contains("clean"), "should show changes, got: {msg}");
    }

    #[test]
    fn status_errors_without_init() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let result = status(&root);
        assert!(result.is_err());
    }

    #[test]
    fn remote_sets_origin() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        let msg = remote(&root, Some("https://example.com/state.git")).unwrap();
        assert!(msg.contains("https://example.com/state.git"));

        let pm_dir = paths::pm_dir(&root);
        assert!(git::has_remote(&pm_dir, "origin").unwrap());
    }

    #[test]
    fn remote_errors_if_already_set() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        remote(&root, Some("https://example.com/state.git")).unwrap();
        let result = remote(&root, Some("https://other.com/state.git"));
        assert!(result.is_err());
    }

    #[test]
    fn push_without_remote_commits_locally() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        // Make a change with no remote configured.
        std::fs::write(root.join(".pm").join("features").join("test.toml"), "x").unwrap();

        let msg = push(&root).unwrap();
        assert!(msg.contains("locally"), "unexpected message: {msg}");
        assert!(msg.contains("no remote configured"), "unexpected: {msg}");

        // The change should have been committed (status clean).
        let st = status(&root).unwrap();
        assert!(st.contains("clean"), "change should be committed: {st}");
    }

    #[test]
    fn push_without_remote_no_changes_is_noop() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        let msg = push(&root).unwrap();
        assert!(msg.contains("no new changes"), "unexpected: {msg}");
        assert!(msg.contains("no remote configured"), "unexpected: {msg}");
    }

    #[test]
    fn pull_without_remote_is_noop() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        let msg = pull(&root).unwrap();
        assert!(msg.contains("nothing to pull"), "unexpected: {msg}");
    }

    #[test]
    fn status_without_remote_notes_it() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        let msg = status(&root).unwrap();
        assert!(msg.contains("no remote configured"), "unexpected: {msg}");
    }

    #[test]
    fn push_with_configured_remote_surfaces_real_errors() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        // Point origin at a remote that doesn't exist — push must fail.
        let pm_dir = paths::pm_dir(&root);
        let bogus = dir.path().join("does-not-exist.git");
        git::add_remote(&pm_dir, "origin", &bogus.to_string_lossy()).unwrap();

        std::fs::write(root.join(".pm").join("features").join("test.toml"), "x").unwrap();

        let result = push(&root);
        assert!(result.is_err(), "push to a broken remote should error");
    }

    #[test]
    fn push_commits_and_pushes() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        // Create a bare remote
        let bare = dir.path().join("state-remote.git");
        git::init_bare(&bare).unwrap();

        let pm_dir = paths::pm_dir(&root);
        git::add_remote(&pm_dir, "origin", &bare.to_string_lossy()).unwrap();

        // Push initial state
        let branch = git::current_branch(&pm_dir).unwrap();
        git::push(&pm_dir, "origin", &branch).unwrap();

        // Make a change
        std::fs::write(root.join(".pm").join("features").join("test.toml"), "x").unwrap();

        let msg = push(&root).unwrap();
        assert!(msg.contains("Committed and pushed"));
    }

    #[test]
    fn push_without_changes_still_pushes() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        // Create a bare remote
        let bare = dir.path().join("state-remote.git");
        git::init_bare(&bare).unwrap();

        let pm_dir = paths::pm_dir(&root);
        git::add_remote(&pm_dir, "origin", &bare.to_string_lossy()).unwrap();

        // Push initial state
        let branch = git::current_branch(&pm_dir).unwrap();
        git::push(&pm_dir, "origin", &branch).unwrap();

        let msg = push(&root).unwrap();
        assert!(msg.contains("no new changes"));
    }

    #[test]
    fn pull_fetches_remote_changes() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        // Create bare remote and push
        let bare = dir.path().join("state-remote.git");
        git::init_bare(&bare).unwrap();
        let pm_dir = paths::pm_dir(&root);
        git::add_remote(&pm_dir, "origin", &bare.to_string_lossy()).unwrap();
        let branch = git::current_branch(&pm_dir).unwrap();
        git::push(&pm_dir, "origin", &branch).unwrap();

        // Clone bare elsewhere, push a change
        let other = dir.path().join("other-clone");
        git::clone_repo(&bare.to_string_lossy(), &other).unwrap();
        std::fs::write(other.join("extra.txt"), "remote data").unwrap();
        git::add_all(&other).unwrap();
        git::commit_with_message(&other, "remote change").unwrap();
        let other_branch = git::current_branch(&other).unwrap();
        git::push(&other, "origin", &other_branch).unwrap();

        // Pull
        let msg = pull(&root).unwrap();
        assert!(msg.contains("Pulled"));

        // Verify the file arrived
        assert!(pm_dir.join("extra.txt").exists());
    }

    #[test]
    fn pull_commits_dirty_state_before_pulling() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        // Create bare remote and push
        let bare = dir.path().join("state-remote.git");
        git::init_bare(&bare).unwrap();
        let pm_dir = paths::pm_dir(&root);
        git::add_remote(&pm_dir, "origin", &bare.to_string_lossy()).unwrap();
        let branch = git::current_branch(&pm_dir).unwrap();
        git::push(&pm_dir, "origin", &branch).unwrap();

        // Make a local dirty change
        std::fs::write(root.join(".pm").join("features").join("dirty.toml"), "x").unwrap();

        // Pull should succeed (auto-commits dirty state first)
        let msg = pull(&root).unwrap();
        assert!(msg.contains("Pulled"));

        // The dirty file should be committed (status clean)
        let st = status(&root).unwrap();
        assert!(
            st.contains("clean"),
            "dirty state should have been committed: {st}"
        );
    }

    #[test]
    fn init_commits_existing_state() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Create some state before init
        std::fs::write(
            root.join(".pm").join("features").join("login.toml"),
            "[feature]\nname = \"login\"\n",
        )
        .unwrap();

        init(&root).unwrap();

        // Verify the state was committed (status should be clean)
        let msg = status(&root).unwrap();
        assert!(msg.contains("clean"), "state should be committed: {msg}");
    }

    #[test]
    fn derive_project_name_from_dir() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("my-cool-project");
        std::fs::create_dir_all(&root).unwrap();

        let name = derive_project_name(&root);
        assert_eq!(name, "my-cool-project");
    }

    fn remote_branches(bare: &std::path::Path) -> Vec<String> {
        git::run_git(
            bare,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
    }

    /// After connecting to a remote whose only branch is `master`, the state
    /// repo must work on `master` and push there — never grow an `origin/main`.
    fn assert_tracks_master_remote(root: &std::path::Path, bare: &std::path::Path) {
        let pm_dir = paths::pm_dir(root);
        assert_eq!(git::current_branch(&pm_dir).unwrap(), "master");
        assert!(pm_dir.join("remote-file.txt").exists());

        std::fs::write(root.join(".pm").join("features").join("t.toml"), "x").unwrap();
        let msg = push(root).unwrap();
        assert!(msg.contains("Committed and pushed"), "unexpected: {msg}");

        assert_eq!(remote_branches(bare), vec!["master".to_string()]);
        assert_eq!(
            git::run_git(bare, &["rev-parse", "master"]).unwrap(),
            git::run_git(&pm_dir, &["rev-parse", "HEAD"]).unwrap()
        );
    }

    #[test]
    fn init_with_remote_on_master_only_remote_tracks_master() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let bare = dir.path().join("state-remote.git");
        create_populated_bare_on(&bare, "master");

        init_inner(&root, false, Some(&bare.to_string_lossy())).unwrap();

        assert_tracks_master_remote(&root, &bare);
    }

    #[test]
    fn init_with_remote_on_existing_repo_master_only_remote_tracks_master() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();
        let bare = dir.path().join("state-remote.git");
        create_populated_bare_on(&bare, "master");

        init_inner(&root, false, Some(&bare.to_string_lossy())).unwrap();

        assert_tracks_master_remote(&root, &bare);
    }

    // -- init --remote tests (project-level) --

    #[test]
    fn init_with_remote_empty_bare() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Empty bare remote — no content to pull
        let bare = dir.path().join("state-remote.git");
        git::init_bare(&bare).unwrap();

        let msg = init_inner(&root, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(msg.contains("Initialised"));
        // Empty remote: message says "remote is empty"
        assert!(
            msg.contains("remote is empty") || msg.contains("pulled"),
            "unexpected message: {msg}"
        );

        let pm_dir = paths::pm_dir(&root);
        assert!(git::has_remote(&pm_dir, "origin").unwrap());
    }

    #[test]
    fn init_with_remote_populated_bare() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Populated remote — has content
        let bare = dir.path().join("state-remote.git");
        create_populated_bare(&bare);

        let msg = init_inner(&root, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(msg.contains("Initialised"), "unexpected message: {msg}");
        assert!(msg.contains("pulled"), "unexpected message: {msg}");

        // Remote content should be present locally
        let pm_dir = paths::pm_dir(&root);
        assert!(git::has_remote(&pm_dir, "origin").unwrap());
        assert!(
            pm_dir.join("remote-file.txt").exists(),
            "remote content should have been pulled"
        );
    }

    #[test]
    fn init_with_remote_on_existing_repo_without_remote() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Init first without remote
        init(&root).unwrap();

        // Create an empty bare remote
        let bare = dir.path().join("state-remote.git");
        git::init_bare(&bare).unwrap();

        let msg = init_inner(&root, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(msg.contains("already initialised"));

        let pm_dir = paths::pm_dir(&root);
        assert!(git::has_remote(&pm_dir, "origin").unwrap());
    }

    #[test]
    fn init_with_remote_on_existing_repo_populated_bare() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Init first without remote
        init(&root).unwrap();

        // Create a populated bare remote
        let bare = dir.path().join("state-remote.git");
        create_populated_bare(&bare);

        let msg = init_inner(&root, false, Some(&bare.to_string_lossy())).unwrap();
        assert!(msg.contains("already initialised"), "unexpected: {msg}");

        let pm_dir = paths::pm_dir(&root);
        assert!(git::has_remote(&pm_dir, "origin").unwrap());
        // Remote content should be present (reset to remote on diverge)
        assert!(
            pm_dir.join("remote-file.txt").exists(),
            "remote content should have been pulled/reset"
        );
    }

    #[test]
    fn init_with_remote_errors_when_origin_already_set() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Init with remote
        let bare = dir.path().join("state-remote.git");
        git::init_bare(&bare).unwrap();
        init_inner(&root, false, Some(&bare.to_string_lossy())).unwrap();

        // Try again with a different remote
        let bare2 = dir.path().join("other-remote.git");
        git::init_bare(&bare2).unwrap();
        let result = init_inner(&root, false, Some(&bare2.to_string_lossy()));
        assert!(result.is_err());
    }
}
