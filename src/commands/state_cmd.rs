use std::io::{self, Write};
use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;
use crate::state::paths;

/// Ensure the .pm/ state repo exists and return its path.
fn require_state_repo(project_root: &Path) -> Result<std::path::PathBuf> {
    let pm_dir = paths::pm_dir(project_root);
    if !pm_dir.join(".git").exists() {
        return Err(PmError::Git(
            "state repo not initialised (run `pm state init`)".to_string(),
        ));
    }
    Ok(pm_dir)
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

/// Remote setup choices.
enum RemoteChoice {
    GitHub,
    Url(String),
    Skip,
}

/// Prompt the user to set up a remote for the state repo.
/// Returns None if skipped, or a message describing what was done.
fn prompt_remote_setup(project_root: &Path, pm_dir: &Path) -> Result<Option<String>> {
    let gh_available = crate::gh::is_available();

    eprintln!("Back up project state to a remote?");
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

    let choice = if answer.is_empty() {
        if gh_available {
            RemoteChoice::GitHub
        } else {
            RemoteChoice::Skip
        }
    } else {
        match answer {
            "1" if gh_available => RemoteChoice::GitHub,
            "2" => {
                eprint!("Remote URL: ");
                io::stderr().flush()?;
                let mut url = String::new();
                io::stdin().read_line(&mut url)?;
                let url = url.trim().to_string();
                if url.is_empty() {
                    RemoteChoice::Skip
                } else {
                    RemoteChoice::Url(url)
                }
            }
            _ => RemoteChoice::Skip,
        }
    };

    match choice {
        RemoteChoice::GitHub => {
            let project_name = derive_project_name(project_root);
            let repo_name = format!("{project_name}-pm-state");
            eprintln!("Creating private repo '{repo_name}'...");
            let url = crate::gh::create_private_repo(&repo_name)?;
            git::add_remote(pm_dir, "origin", &url)?;
            // Push initial state
            let branch = git::current_branch(pm_dir)?;
            git::push(pm_dir, "origin", &branch)?;
            Ok(Some(format!("Created GitHub repo and pushed: {url}")))
        }
        RemoteChoice::Url(url) => {
            git::add_remote(pm_dir, "origin", &url)?;
            Ok(Some(format!("Set state remote to {url}")))
        }
        RemoteChoice::Skip => Ok(None),
    }
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

/// Set the remote URL for the state repo.
pub fn remote(project_root: &Path, url: &str) -> Result<String> {
    let pm_dir = require_state_repo(project_root)?;

    if git::has_remote(&pm_dir, "origin")? {
        return Err(PmError::Git(
            "remote 'origin' already exists (remove it with `git -C .pm remote remove origin` to reset)".to_string(),
        ));
    }

    git::add_remote(&pm_dir, "origin", url)?;
    Ok(format!("Set state remote to {url}"))
}

/// Auto-commit and push the state repo.
pub fn push(project_root: &Path) -> Result<String> {
    let pm_dir = require_state_repo(project_root)?;

    if !git::has_remote(&pm_dir, "origin")? {
        return Err(PmError::Git(
            "no remote configured (run `pm state remote <url>`)".to_string(),
        ));
    }

    // Stage and commit any changes
    git::add_all(&pm_dir)?;
    let committed = if git::has_staged_changes(&pm_dir)? {
        let changed = git::staged_file_names(&pm_dir)?;
        let msg = if changed.is_empty() {
            "state sync".to_string()
        } else {
            format!("state sync ({})", changed.join(", "))
        };
        git::commit_with_message(&pm_dir, &msg)?;
        true
    } else {
        false
    };

    // Push
    let branch = git::current_branch(&pm_dir)?;
    git::push(&pm_dir, "origin", &branch)?;

    if committed {
        Ok("Committed and pushed state".to_string())
    } else {
        Ok("Pushed state (no new changes to commit)".to_string())
    }
}

/// Pull state from the remote.
///
/// Commits any local changes first so `git pull --ff-only` doesn't
/// fail on a dirty working tree.
pub fn pull(project_root: &Path) -> Result<String> {
    let pm_dir = require_state_repo(project_root)?;

    if !git::has_remote(&pm_dir, "origin")? {
        return Err(PmError::Git(
            "no remote configured (run `pm state remote <url>`)".to_string(),
        ));
    }

    // Commit any dirty state before pulling to avoid conflicts
    commit_if_dirty(&pm_dir)?;

    match git::pull(&pm_dir) {
        Ok(()) => Ok("Pulled state from remote".to_string()),
        Err(e) => {
            let _ = git::merge_abort(&pm_dir);
            Err(PmError::Git(format!("state pull failed: {e}")))
        }
    }
}

/// Stage all changes and commit if there's anything to commit.
fn commit_if_dirty(pm_dir: &Path) -> Result<()> {
    git::add_all(pm_dir)?;
    if git::has_staged_changes(pm_dir)? {
        let changed = git::staged_file_names(pm_dir)?;
        let msg = if changed.is_empty() {
            "state sync (pre-pull)".to_string()
        } else {
            format!("state sync ({})", changed.join(", "))
        };
        git::commit_with_message(pm_dir, &msg)?;
    }
    Ok(())
}

/// Show git status of the state repo.
pub fn status(project_root: &Path) -> Result<String> {
    let pm_dir = require_state_repo(project_root)?;
    let output = git::status_short(&pm_dir)?;
    if output.is_empty() {
        Ok("State repo is clean".to_string())
    } else {
        Ok(output)
    }
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

        let msg = remote(&root, "https://example.com/state.git").unwrap();
        assert!(msg.contains("https://example.com/state.git"));

        let pm_dir = paths::pm_dir(&root);
        assert!(git::has_remote(&pm_dir, "origin").unwrap());
    }

    #[test]
    fn remote_errors_if_already_set() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        init(&root).unwrap();

        remote(&root, "https://example.com/state.git").unwrap();
        let result = remote(&root, "https://other.com/state.git");
        assert!(result.is_err());
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
}
