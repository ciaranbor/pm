use std::fmt::Write;
use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;
use crate::messages;
use crate::state::paths;

struct DefaultCategory {
    filename: &'static str,
    heading: &'static str,
    description: &'static str,
}

const DEFAULT_CATEGORIES: &[DefaultCategory] = &[
    DefaultCategory {
        filename: "todo.md",
        heading: "# Todo",
        description: "Ordered task list. Actionable items with clear next steps.",
    },
    DefaultCategory {
        filename: "issues.md",
        heading: "# Issues",
        description: "Concrete bugs and unexpected behaviours discovered during usage.",
    },
    DefaultCategory {
        filename: "ideas.md",
        heading: "# Ideas",
        description: "Thoughts and design questions that aren't yet actionable.",
    },
];

/// Generate `categories.toml` content from the default categories.
fn default_categories_toml() -> String {
    let mut out = String::new();
    for (i, cat) in DEFAULT_CATEGORIES.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        writeln!(out, "[[category]]").unwrap();
        writeln!(out, "filename = \"{}\"", cat.filename).unwrap();
        writeln!(out, "description = \"{}\"", cat.description).unwrap();
    }
    out
}

fn require_state_repo(project_root: &Path) -> Result<std::path::PathBuf> {
    let pm_dir = paths::pm_dir(project_root);
    if !pm_dir.join(".git").exists() {
        return Err(PmError::Git(
            "state repo not initialised (run `pm state init`)".to_string(),
        ));
    }
    Ok(pm_dir)
}

/// Bootstrap the information store at `.pm/docs/`.
///
/// Creates the directory and writes default `categories.toml` and category
/// markdown files. Does NOT create a separate git repo — docs are tracked
/// by the parent `.pm/` state repo (initialised by `pm state init`).
/// Idempotent — won't overwrite existing files.
pub fn bootstrap(project_root: &Path) -> Result<()> {
    let docs_dir = paths::docs_dir(project_root);

    std::fs::create_dir_all(&docs_dir)?;

    // Write categories.toml (only if it doesn't exist, to preserve customisations)
    let categories_path = docs_dir.join("categories.toml");
    if !categories_path.exists() {
        std::fs::write(&categories_path, default_categories_toml())?;
    }

    // Write default category files
    for cat in DEFAULT_CATEGORIES {
        let cat_path = docs_dir.join(cat.filename);
        if !cat_path.exists() {
            std::fs::write(&cat_path, format!("{}\n", cat.heading))?;
        }
    }

    Ok(())
}

/// Stage all changes and commit if there's anything to commit.
/// Returns `true` if a commit was created.
fn commit_staged(pm_dir: &Path) -> Result<bool> {
    git::add_all(pm_dir)?;
    if git::has_staged_changes(pm_dir)? {
        let changed = git::staged_file_names(pm_dir)?;
        let msg = if changed.is_empty() {
            "sync".to_string()
        } else {
            format!("sync ({})", changed.join(", "))
        };
        git::commit_with_message(pm_dir, &msg)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Sync the information store: commit changes in `.pm/` and push if remote configured.
///
/// Operates on the `.pm/` state repo. If a remote is configured, pulls first
/// then pushes after committing. On pull conflict, aborts and notifies the
/// main agent.
pub fn sync(project_root: &Path) -> Result<String> {
    let pm_dir = require_state_repo(project_root)?;
    let has_remote = git::has_remote(&pm_dir, "origin")?;

    let mut committed = false;

    // Pull first if remote exists
    if has_remote {
        // Commit any local changes before pulling to avoid losing them
        if commit_staged(&pm_dir)? {
            committed = true;
        }

        if let Err(e) = git::pull(&pm_dir) {
            let _ = git::merge_abort(&pm_dir);
            notify_conflict(project_root, &format!("docs sync pull failed: {e}"))?;
            return Err(PmError::Git(format!(
                "pull failed — message sent to main agent: {e}"
            )));
        }
    }

    // Stage and commit any remaining changes (or changes from a no-remote flow)
    if commit_staged(&pm_dir)? {
        committed = true;
    }

    // Push if remote exists and we have something new
    if has_remote && committed {
        let branch = git::current_branch(&pm_dir)?;
        if let Err(e) = git::push(&pm_dir, "origin", &branch) {
            return Err(PmError::Git(format!("push failed: {e}")));
        }
        Ok("Synced information store (pushed to remote)".to_string())
    } else if committed {
        Ok("Synced information store".to_string())
    } else {
        Ok("Nothing to sync".to_string())
    }
}

/// Send a message to the main agent about a docs conflict.
fn notify_conflict(project_root: &Path, detail: &str) -> Result<()> {
    let messages_dir = paths::messages_dir(project_root);
    let body = format!(
        "## Information store sync failed\n\n\
         The docs sync pull failed and was aborted.\n\
         Local changes are preserved but not pushed.\n\n\
         Detail: {detail}\n\n\
         To resolve: `cd .pm && git pull` and fix any conflicts manually, \
         then `pm docs sync`."
    );
    messages::send(&messages_dir, "main", "main", "pm", &body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::state_cmd;
    use tempfile::tempdir;

    fn setup_project(dir: &std::path::Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm").join("features")).unwrap();
        std::fs::create_dir_all(root.join("main")).unwrap();
        root
    }

    /// Setup a project with state repo initialised (required for docs sync).
    fn setup_with_state(dir: &std::path::Path) -> std::path::PathBuf {
        let root = setup_project(dir);
        state_cmd::init(&root).unwrap();
        bootstrap(&root).unwrap();
        // Commit the bootstrapped docs
        let pm_dir = paths::pm_dir(&root);
        git::add_all(&pm_dir).unwrap();
        if git::has_staged_changes(&pm_dir).unwrap() {
            git::commit_with_message(&pm_dir, "bootstrap docs").unwrap();
        }
        root
    }

    #[test]
    fn bootstrap_creates_docs_directory() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        bootstrap(&root).unwrap();

        assert!(paths::docs_dir(&root).exists());
    }

    #[test]
    fn bootstrap_creates_categories_toml() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        bootstrap(&root).unwrap();

        let categories_path = paths::docs_dir(&root).join("categories.toml");
        assert!(categories_path.exists());

        let content = std::fs::read_to_string(&categories_path).unwrap();
        assert!(content.contains("todo.md"));
        assert!(content.contains("issues.md"));
        assert!(content.contains("ideas.md"));
    }

    #[test]
    fn bootstrap_creates_default_category_files() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        bootstrap(&root).unwrap();

        let docs = paths::docs_dir(&root);
        assert!(docs.join("todo.md").exists());
        assert!(docs.join("issues.md").exists());
        assert!(docs.join("ideas.md").exists());

        let todo = std::fs::read_to_string(docs.join("todo.md")).unwrap();
        assert!(todo.starts_with("# Todo"));
    }

    #[test]
    fn bootstrap_does_not_create_git_repo() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        bootstrap(&root).unwrap();

        // No separate git repo in docs
        assert!(!paths::docs_dir(&root).join(".git").exists());
    }

    #[test]
    fn bootstrap_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        bootstrap(&root).unwrap();

        // Modify a file to verify bootstrap doesn't overwrite
        let docs = paths::docs_dir(&root);
        std::fs::write(docs.join("todo.md"), "# Todo\n- custom item\n").unwrap();

        bootstrap(&root).unwrap();

        // Content should be preserved
        let content = std::fs::read_to_string(docs.join("todo.md")).unwrap();
        assert!(content.contains("custom item"));
    }

    #[test]
    fn sync_commits_changes() {
        let dir = tempdir().unwrap();
        let root = setup_with_state(dir.path());

        // Write to a file
        let docs = paths::docs_dir(&root);
        std::fs::write(docs.join("todo.md"), "# Todo\n- new task\n").unwrap();

        let msg = sync(&root).unwrap();
        assert_eq!(msg, "Synced information store");

        // Verify latest commit message includes file name
        let pm_dir = paths::pm_dir(&root);
        let output = std::process::Command::new("git")
            .args(["-C", &pm_dir.to_string_lossy(), "log", "--oneline", "-1"])
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(
            log.contains("todo.md"),
            "commit message should include changed file name, got: {log}"
        );
    }

    #[test]
    fn sync_with_no_changes_succeeds() {
        let dir = tempdir().unwrap();
        let root = setup_with_state(dir.path());

        let msg = sync(&root).unwrap();
        assert_eq!(msg, "Nothing to sync");
    }

    #[test]
    fn sync_without_state_init_returns_error() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let result = sync(&root);
        assert!(result.is_err());
    }

    /// Helper: create a bare remote repo and push the state repo to it.
    fn setup_remote(root: &Path) -> std::path::PathBuf {
        let pm_dir = paths::pm_dir(root);
        let bare_path = root.join("remote-state.git");
        git::init_bare(&bare_path).unwrap();
        git::add_remote(&pm_dir, "origin", &bare_path.to_string_lossy()).unwrap();
        let branch = git::current_branch(&pm_dir).unwrap();
        git::push(&pm_dir, "origin", &branch).unwrap();
        bare_path
    }

    #[test]
    fn sync_with_remote_pushes() {
        let dir = tempdir().unwrap();
        let root = setup_with_state(dir.path());
        let _bare = setup_remote(&root);

        let docs = paths::docs_dir(&root);
        std::fs::write(docs.join("todo.md"), "# Todo\n- pushed task\n").unwrap();

        let msg = sync(&root).unwrap();
        assert!(
            msg.contains("pushed to remote"),
            "expected push confirmation, got: {msg}"
        );
    }

    #[test]
    fn sync_with_remote_no_changes_skips_push() {
        let dir = tempdir().unwrap();
        let root = setup_with_state(dir.path());
        let _bare = setup_remote(&root);

        let msg = sync(&root).unwrap();
        assert_eq!(msg, "Nothing to sync");
    }

    #[test]
    fn sync_with_remote_pulls_remote_changes() {
        let dir = tempdir().unwrap();
        let root = setup_with_state(dir.path());
        let bare = setup_remote(&root);

        // Clone the bare repo elsewhere and push a change
        let other = dir.path().join("other-clone");
        git::clone_repo(&bare.to_string_lossy(), &other).unwrap();
        std::fs::write(
            other.join("docs").join("issues.md"),
            "# Issues\n- remote issue\n",
        )
        .unwrap();
        git::add_all(&other).unwrap();
        git::commit_with_message(&other, "remote change").unwrap();
        let branch = git::current_branch(&other).unwrap();
        git::push(&other, "origin", &branch).unwrap();

        // Sync — should pull the remote change (no local changes to push)
        let msg = sync(&root).unwrap();
        assert_eq!(msg, "Nothing to sync");

        // Verify the remote change was pulled in
        let docs = paths::docs_dir(&root);
        let issues = std::fs::read_to_string(docs.join("issues.md")).unwrap();
        assert!(
            issues.contains("remote issue"),
            "remote change should be pulled in"
        );
    }

    #[test]
    fn sync_conflict_sends_message_to_main() {
        let dir = tempdir().unwrap();
        let root = setup_with_state(dir.path());
        let bare = setup_remote(&root);

        // Push a conflicting change from another clone
        let other = dir.path().join("other-clone");
        git::clone_repo(&bare.to_string_lossy(), &other).unwrap();
        std::fs::write(
            other.join("docs").join("todo.md"),
            "# Todo\n- remote version\n",
        )
        .unwrap();
        git::add_all(&other).unwrap();
        git::commit_with_message(&other, "remote conflicting change").unwrap();
        let branch = git::current_branch(&other).unwrap();
        git::push(&other, "origin", &branch).unwrap();

        // Make a conflicting local change
        let docs = paths::docs_dir(&root);
        std::fs::write(docs.join("todo.md"), "# Todo\n- local version\n").unwrap();

        // Sync should fail and send a message
        let result = sync(&root);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("conflict") || err.contains("pull"));

        // Verify a message was sent to the main agent
        let messages_dir = paths::messages_dir(&root);
        let msgs = messages::list(&messages_dir, "main", "main", None).unwrap();
        assert!(
            !msgs.is_empty(),
            "should have sent a conflict message to main agent"
        );
    }
}
