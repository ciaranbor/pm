use std::io::{self, Write};
use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;
use crate::state::paths;

/// Context for operating on a git-backed state directory.
///
/// Both project-level (`.pm/`) and global (`~/.config/pm/`) state repos
/// use the same logic — only labels and hint messages differ.
struct RepoContext<'a> {
    dir: &'a Path,
    label: &'a str,
    init_hint: &'a str,
    remote_hint: &'a str,
}

/// Verify the directory has a git repo.
fn require_repo(ctx: &RepoContext) -> Result<()> {
    if !ctx.dir.join(".git").exists() {
        return Err(PmError::Git(format!(
            "{} repo not initialised (run `{}`)",
            ctx.label, ctx.init_hint
        )));
    }
    Ok(())
}

/// Set the remote URL for a state repo.
fn set_remote(ctx: &RepoContext, url: &str) -> Result<String> {
    require_repo(ctx)?;

    if git::has_remote(ctx.dir, "origin")? {
        return Err(PmError::Git(format!(
            "remote 'origin' already exists (remove it with `git -C {} remote remove origin` to reset)",
            ctx.dir.display()
        )));
    }

    git::add_remote(ctx.dir, "origin", url)?;
    Ok(format!("Set {} remote to {url}", ctx.label))
}

/// Auto-commit and push a state repo.
fn push_repo(ctx: &RepoContext) -> Result<String> {
    require_repo(ctx)?;

    if !git::has_remote(ctx.dir, "origin")? {
        return Err(PmError::Git(format!(
            "no remote configured (run `{}`)",
            ctx.remote_hint
        )));
    }

    git::add_all(ctx.dir)?;
    let committed = if git::has_staged_changes(ctx.dir)? {
        let changed = git::staged_file_names(ctx.dir)?;
        let msg = if changed.is_empty() {
            format!("{} sync", ctx.label)
        } else {
            format!("{} sync ({})", ctx.label, changed.join(", "))
        };
        git::commit_with_message(ctx.dir, &msg)?;
        true
    } else {
        false
    };

    let branch = git::current_branch(ctx.dir)?;
    git::push(ctx.dir, "origin", &branch)?;

    if committed {
        Ok(format!("Committed and pushed {}", ctx.label))
    } else {
        Ok(format!("Pushed {} (no new changes to commit)", ctx.label))
    }
}

/// Pull state from the remote, auto-committing dirty state first.
fn pull_repo(ctx: &RepoContext) -> Result<String> {
    require_repo(ctx)?;

    if !git::has_remote(ctx.dir, "origin")? {
        return Err(PmError::Git(format!(
            "no remote configured (run `{}`)",
            ctx.remote_hint
        )));
    }

    commit_if_dirty(ctx)?;

    match git::pull(ctx.dir) {
        Ok(()) => Ok(format!("Pulled {} from remote", ctx.label)),
        Err(e) => {
            let _ = git::merge_abort(ctx.dir);
            Err(PmError::Git(format!("{} pull failed: {e}", ctx.label)))
        }
    }
}

/// Show git status of a state repo.
fn status_repo(ctx: &RepoContext) -> Result<String> {
    require_repo(ctx)?;
    let output = git::status_short(ctx.dir)?;
    if output.is_empty() {
        Ok(format!("{} repo is clean", capitalize(ctx.label)))
    } else {
        Ok(output)
    }
}

/// Stage all changes and commit if there's anything to commit.
fn commit_if_dirty(ctx: &RepoContext) -> Result<()> {
    git::add_all(ctx.dir)?;
    if git::has_staged_changes(ctx.dir)? {
        let changed = git::staged_file_names(ctx.dir)?;
        let msg = if changed.is_empty() {
            format!("{} sync (pre-pull)", ctx.label)
        } else {
            format!("{} sync ({})", ctx.label, changed.join(", "))
        };
        git::commit_with_message(ctx.dir, &msg)?;
    }
    Ok(())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
}

// ---------------------------------------------------------------------------
// Project-level state repo (.pm/)
// ---------------------------------------------------------------------------

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
    init_inner(project_root, false)
}

/// Initialise with interactive remote setup prompt.
pub fn init_interactive(project_root: &Path) -> Result<String> {
    init_inner(project_root, true)
}

fn init_inner(project_root: &Path, interactive: bool) -> Result<String> {
    let pm_dir = paths::pm_dir(project_root);

    if pm_dir.join(".git").exists() {
        // Already initialised — but if interactive and no remote, offer remote setup
        if interactive && !git::has_remote(&pm_dir, "origin")? {
            let mut result = "State repo already initialised".to_string();
            if let Some(remote_msg) = prompt_remote_setup(project_root, &pm_dir)? {
                result.push('\n');
                result.push_str(&remote_msg);
            }
            return Ok(result);
        }
        return Ok("State repo already initialised".to_string());
    }

    if !pm_dir.exists() {
        return Err(PmError::Git(
            ".pm/ directory does not exist — is this a pm project?".to_string(),
        ));
    }

    // Init the repo (creates initial empty commit)
    git::init_repo(&pm_dir)?;

    // Stage everything and commit the current state
    git::add_all(&pm_dir)?;
    if git::has_staged_changes(&pm_dir)? {
        git::commit_with_message(&pm_dir, "init state repo")?;
    }

    let mut result = "Initialised state repo in .pm/".to_string();

    // Interactive remote setup
    if interactive && let Some(remote_msg) = prompt_remote_setup(project_root, &pm_dir)? {
        result.push('\n');
        result.push_str(&remote_msg);
    }

    Ok(result)
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

// ---------------------------------------------------------------------------
// Global registry repo (~/.config/pm/)
// ---------------------------------------------------------------------------

/// Default .gitignore for the global registry.
///
/// The project-level `.pm/` directory doesn't need a `.gitignore` because
/// pm controls all files there. The global registry may accumulate
/// machine-specific ephemeral files (lock files, pid files) that should
/// not be committed.
const GLOBAL_GITIGNORE: &str = "\
# Ephemeral / machine-specific state
*.lock
*.pid
";

fn global_ctx(dir: &Path) -> RepoContext<'_> {
    RepoContext {
        dir,
        label: "global registry",
        init_hint: "pm state init --global",
        remote_hint: "pm state remote --global <url>",
    }
}

/// Initialise a git repo in ~/.config/pm/ for global registry backup.
/// Non-interactive variant for programmatic use (e.g. `pm upgrade`).
pub fn global_init() -> Result<String> {
    let dir = paths::global_config_dir()?;
    global_init_at(&dir, false)
}

/// Initialise with interactive remote setup prompt.
pub fn global_init_interactive() -> Result<String> {
    let dir = paths::global_config_dir()?;
    global_init_at(&dir, true)
}

fn global_init_at(dir: &Path, interactive: bool) -> Result<String> {
    if dir.join(".git").exists() {
        return Ok("Global registry repo already initialised".to_string());
    }

    if !dir.exists() {
        return Err(PmError::Git(
            "~/.config/pm/ does not exist — run `pm init` first to create a project".to_string(),
        ));
    }

    // Write .gitignore before first commit
    let gitignore_path = dir.join(".gitignore");
    if !gitignore_path.exists() {
        std::fs::write(&gitignore_path, GLOBAL_GITIGNORE)?;
    }

    git::init_repo(dir)?;

    // Stage everything and commit
    git::add_all(dir)?;
    if git::has_staged_changes(dir)? {
        git::commit_with_message(dir, "init global registry repo")?;
    }

    let mut result = format!("Initialised global registry repo in {}", dir.display());

    if interactive
        && let Some(remote_msg) =
            prompt_remote_setup_common(dir, "global registry", "pm-global-registry")?
    {
        result.push('\n');
        result.push_str(&remote_msg);
    }

    Ok(result)
}

/// Set the remote URL for the global registry repo.
pub fn global_remote(url: &str) -> Result<String> {
    let dir = paths::global_config_dir()?;
    set_remote(&global_ctx(&dir), url)
}

/// Auto-commit and push the global registry repo.
pub fn global_push() -> Result<String> {
    let dir = paths::global_config_dir()?;
    push_repo(&global_ctx(&dir))
}

/// Pull global registry from the remote.
pub fn global_pull() -> Result<String> {
    let dir = paths::global_config_dir()?;
    pull_repo(&global_ctx(&dir))
}

/// Show git status of the global registry repo.
pub fn global_status() -> Result<String> {
    let dir = paths::global_config_dir()?;
    status_repo(&global_ctx(&dir))
}

// ---------------------------------------------------------------------------
// Interactive remote setup (shared)
// ---------------------------------------------------------------------------

/// Remote setup choices.
enum RemoteChoice {
    GitHub,
    Url(String),
    Skip,
}

/// Read the user's remote setup choice from stdin.
fn read_remote_choice() -> Result<RemoteChoice> {
    let gh_available = crate::gh::is_available();

    if gh_available {
        eprintln!("  1) Create a private GitHub repo");
    }
    eprintln!("  2) Use an existing URL");
    eprintln!("  3) Skip (local only)");
    eprint!("Choice [{}]: ", if gh_available { "1" } else { "3" });
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim();

    if answer.is_empty() {
        return Ok(if gh_available {
            RemoteChoice::GitHub
        } else {
            RemoteChoice::Skip
        });
    }

    match answer {
        "1" if gh_available => Ok(RemoteChoice::GitHub),
        "2" => {
            eprint!("Remote URL: ");
            io::stderr().flush()?;
            let mut url = String::new();
            io::stdin().read_line(&mut url)?;
            let url = url.trim().to_string();
            if url.is_empty() {
                Ok(RemoteChoice::Skip)
            } else {
                Ok(RemoteChoice::Url(url))
            }
        }
        _ => Ok(RemoteChoice::Skip),
    }
}

/// Shared remote setup prompt. `what` describes what's being backed up
/// (e.g. "project state", "global registry"). `gh_repo_name` is used
/// when the user chooses to create a GitHub repo.
fn prompt_remote_setup_common(
    dir: &Path,
    what: &str,
    gh_repo_name: &str,
) -> Result<Option<String>> {
    eprintln!("Back up {what} to a remote?");
    let choice = read_remote_choice()?;

    match choice {
        RemoteChoice::GitHub => {
            eprintln!("Creating private repo '{gh_repo_name}'...");
            let url = crate::gh::create_private_repo(gh_repo_name)?;
            git::add_remote(dir, "origin", &url)?;
            let branch = git::current_branch(dir)?;
            git::push(dir, "origin", &branch)?;
            Ok(Some(format!("Created GitHub repo and pushed: {url}")))
        }
        RemoteChoice::Url(url) => {
            git::add_remote(dir, "origin", &url)?;
            Ok(Some(format!("Set remote to {url}")))
        }
        RemoteChoice::Skip => Ok(None),
    }
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

// ---------------------------------------------------------------------------
// Registry URL persistence helpers
// ---------------------------------------------------------------------------

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

/// Backfill `repo_url` and `state_remote` for all registry entries by reading
/// the actual git remotes from each project's main worktree and .pm/ directory.
pub fn backfill() -> Result<Vec<String>> {
    let projects_dir = paths::global_projects_dir()?;
    backfill_with_dir(&projects_dir)
}

/// Testable inner function that takes an explicit projects directory.
pub fn backfill_with_dir(projects_dir: &Path) -> Result<Vec<String>> {
    let projects = crate::state::project::ProjectEntry::list(projects_dir)?;
    let mut messages = Vec::new();

    for (name, mut entry) in projects {
        let root = entry.root_path();
        if !root.exists() {
            messages.push(format!("{name}: skipped (root does not exist)"));
            continue;
        }

        let mut changed = false;

        // Backfill repo_url from main worktree's origin
        if entry.repo_url.is_none() {
            let main_path = root.join("main");
            if git::is_git_repo(&main_path)
                && let Ok(Some(url)) = git::remote_url(&main_path, "origin")
            {
                entry.repo_url = Some(url.clone());
                changed = true;
                messages.push(format!("{name}: set repo_url = {url}"));
            }
        }

        // Backfill state_remote from .pm/'s origin
        if entry.state_remote.is_none() {
            let pm_dir = paths::pm_dir(&root);
            if git::is_git_repo(&pm_dir)
                && let Ok(Some(url)) = git::remote_url(&pm_dir, "origin")
            {
                entry.state_remote = Some(url.clone());
                changed = true;
                messages.push(format!("{name}: set state_remote = {url}"));
            }
        }

        if changed {
            entry.save(projects_dir, &name)?;
        } else {
            messages.push(format!("{name}: nothing to backfill"));
        }
    }

    if messages.is_empty() {
        messages.push("No projects in registry".to_string());
    }

    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_project(dir: &std::path::Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm").join("features")).unwrap();
        std::fs::create_dir_all(root.join("main")).unwrap();
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
    fn remote_none_with_closed_stdin_skips() {
        // When stdin is closed (as in tests), read_line returns empty string.
        // If gh is not available, the default choice is Skip.
        if crate::gh::is_available() {
            // Skip: empty stdin + gh available would default to creating a
            // real GitHub repo, which we don't want in tests.
            return;
        }
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        let msg = remote(&root, None).unwrap();
        assert_eq!(msg, "Skipped remote setup");

        // No remote should have been set
        let pm_dir = paths::pm_dir(&root);
        assert!(!git::has_remote(&pm_dir, "origin").unwrap());
    }

    #[test]
    fn push_errors_without_remote() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        let result = push(&root);
        assert!(result.is_err());
    }

    #[test]
    fn pull_errors_without_remote() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        let result = pull(&root);
        assert!(result.is_err());
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

    // -- Global registry tests --

    fn setup_global_dir(dir: &std::path::Path) -> std::path::PathBuf {
        let global = dir.join("config-pm");
        std::fs::create_dir_all(global.join("projects")).unwrap();
        global
    }

    #[test]
    fn global_init_creates_git_repo() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        let msg = global_init_at(&global, false).unwrap();
        assert!(msg.contains("Initialised"));
        assert!(global.join(".git").exists());
        assert!(global.join(".gitignore").exists());
    }

    #[test]
    fn global_init_is_idempotent() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        global_init_at(&global, false).unwrap();
        let msg = global_init_at(&global, false).unwrap();
        assert!(msg.contains("already initialised"));
    }

    #[test]
    fn global_init_errors_without_dir() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("nonexistent");

        let result = global_init_at(&global, false);
        assert!(result.is_err());
    }

    #[test]
    fn global_status_shows_clean_after_init() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        global_init_at(&global, false).unwrap();
        let ctx = global_ctx(&global);
        let msg = status_repo(&ctx).unwrap();
        assert!(msg.contains("clean"));
    }

    #[test]
    fn global_status_shows_changes_after_modification() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        global_init_at(&global, false).unwrap();
        std::fs::write(global.join("projects").join("test.toml"), "x").unwrap();

        let ctx = global_ctx(&global);
        let msg = status_repo(&ctx).unwrap();
        assert!(!msg.contains("clean"), "should show changes, got: {msg}");
    }

    #[test]
    fn global_status_errors_without_init() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        let ctx = global_ctx(&global);
        let result = status_repo(&ctx);
        assert!(result.is_err());
    }

    #[test]
    fn global_remote_sets_origin() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false).unwrap();

        let ctx = global_ctx(&global);
        let msg = set_remote(&ctx, "https://example.com/registry.git").unwrap();
        assert!(msg.contains("https://example.com/registry.git"));
        assert!(git::has_remote(&global, "origin").unwrap());
    }

    #[test]
    fn global_remote_errors_if_already_set() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false).unwrap();

        let ctx = global_ctx(&global);
        set_remote(&ctx, "https://example.com/registry.git").unwrap();
        let result = set_remote(&ctx, "https://other.com/registry.git");
        assert!(result.is_err());
    }

    #[test]
    fn global_push_errors_without_remote() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false).unwrap();

        let ctx = global_ctx(&global);
        let result = push_repo(&ctx);
        assert!(result.is_err());
    }

    #[test]
    fn global_pull_errors_without_remote() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false).unwrap();

        let ctx = global_ctx(&global);
        let result = pull_repo(&ctx);
        assert!(result.is_err());
    }

    #[test]
    fn global_push_commits_and_pushes() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false).unwrap();

        // Create a bare remote
        let bare = dir.path().join("registry-remote.git");
        git::init_bare(&bare).unwrap();
        git::add_remote(&global, "origin", &bare.to_string_lossy()).unwrap();

        // Push initial state
        let branch = git::current_branch(&global).unwrap();
        git::push(&global, "origin", &branch).unwrap();

        // Make a change
        std::fs::write(global.join("projects").join("new.toml"), "x").unwrap();

        let ctx = global_ctx(&global);
        let msg = push_repo(&ctx).unwrap();
        assert!(msg.contains("Committed and pushed"));
    }

    #[test]
    fn global_pull_fetches_remote_changes() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());
        global_init_at(&global, false).unwrap();

        // Create bare remote and push
        let bare = dir.path().join("registry-remote.git");
        git::init_bare(&bare).unwrap();
        git::add_remote(&global, "origin", &bare.to_string_lossy()).unwrap();
        let branch = git::current_branch(&global).unwrap();
        git::push(&global, "origin", &branch).unwrap();

        // Clone bare elsewhere, push a change
        let other = dir.path().join("other-clone");
        git::clone_repo(&bare.to_string_lossy(), &other).unwrap();
        std::fs::write(other.join("extra.txt"), "remote data").unwrap();
        git::add_all(&other).unwrap();
        git::commit_with_message(&other, "remote change").unwrap();
        let other_branch = git::current_branch(&other).unwrap();
        git::push(&other, "origin", &other_branch).unwrap();

        // Pull
        let ctx = global_ctx(&global);
        let msg = pull_repo(&ctx).unwrap();
        assert!(msg.contains("Pulled"));
        assert!(global.join("extra.txt").exists());
    }

    #[test]
    fn global_init_commits_existing_state() {
        let dir = tempdir().unwrap();
        let global = setup_global_dir(dir.path());

        // Create some state before init
        std::fs::write(
            global.join("projects").join("myproject.toml"),
            "[project]\nname = \"myproject\"\n",
        )
        .unwrap();

        global_init_at(&global, false).unwrap();

        let ctx = global_ctx(&global);
        let msg = status_repo(&ctx).unwrap();
        assert!(msg.contains("clean"), "state should be committed: {msg}");
    }

    // -- Backfill tests --

    use crate::state::project::ProjectEntry;

    /// Create a project dir with a main worktree that has a git origin remote.
    fn setup_project_with_origin(root: &std::path::Path, origin_url: &str) {
        let main_path = root.join("main");
        std::fs::create_dir_all(&main_path).unwrap();
        git::init_repo(&main_path).unwrap();
        git::add_remote(&main_path, "origin", origin_url).unwrap();
    }

    /// Create a .pm/ dir with a git repo and origin remote.
    fn setup_pm_with_remote(root: &std::path::Path, remote_url: &str) {
        let pm_dir = root.join(".pm");
        std::fs::create_dir_all(pm_dir.join("features")).unwrap();
        git::init_repo(&pm_dir).unwrap();
        git::add_remote(&pm_dir, "origin", remote_url).unwrap();
    }

    #[test]
    fn backfill_fills_repo_url_from_origin() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let project_root = dir.path().join("myapp");

        setup_project_with_origin(&project_root, "https://github.com/user/myapp.git");

        let entry = ProjectEntry {
            root: project_root.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "myapp").unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(msgs.iter().any(|m| m.contains("set repo_url")), "{msgs:?}");

        let loaded = ProjectEntry::load(&projects_dir, "myapp").unwrap();
        assert_eq!(
            loaded.repo_url.as_deref(),
            Some("https://github.com/user/myapp.git")
        );
    }

    #[test]
    fn backfill_fills_state_remote_from_pm_origin() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let project_root = dir.path().join("myapp");

        std::fs::create_dir_all(&project_root).unwrap();
        setup_pm_with_remote(&project_root, "https://github.com/user/myapp-pm-state.git");

        let entry = ProjectEntry {
            root: project_root.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "myapp").unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("set state_remote")),
            "{msgs:?}"
        );

        let loaded = ProjectEntry::load(&projects_dir, "myapp").unwrap();
        assert_eq!(
            loaded.state_remote.as_deref(),
            Some("https://github.com/user/myapp-pm-state.git")
        );
    }

    #[test]
    fn backfill_skips_entries_already_with_urls() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let project_root = dir.path().join("myapp");

        setup_project_with_origin(&project_root, "https://github.com/user/myapp.git");

        let entry = ProjectEntry {
            root: project_root.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: Some("https://existing.com/repo.git".to_string()),
            state_remote: Some("https://existing.com/state.git".to_string()),
        };
        entry.save(&projects_dir, "myapp").unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("nothing to backfill")),
            "{msgs:?}"
        );

        // URLs should be unchanged
        let loaded = ProjectEntry::load(&projects_dir, "myapp").unwrap();
        assert_eq!(
            loaded.repo_url.as_deref(),
            Some("https://existing.com/repo.git")
        );
    }

    #[test]
    fn backfill_skips_missing_root() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");

        let entry = ProjectEntry {
            root: "/nonexistent/path/myapp".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "myapp").unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.contains("skipped (root does not exist)")),
            "{msgs:?}"
        );
    }

    #[test]
    fn backfill_empty_registry() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(msgs.iter().any(|m| m.contains("No projects")), "{msgs:?}");
    }
}
