//! Connecting a state repo to a remote: non-interactively for `--remote`,
//! or by prompting.

use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;

use super::repo::{RepoContext, commit_if_dirty};

/// Set the remote URL and pull. Used by `--remote` flag on init.
///
/// When `fresh` is true the repo was just created and the remote is
/// authoritative — we fetch and reset to the remote branch instead of
/// trying to merge. When `fresh` is false the repo already has meaningful
/// local commits so we commit dirty state, fetch, and fast-forward pull.
pub(crate) fn apply_remote_and_pull(
    dir: &Path,
    url: &str,
    label: &str,
    fresh: bool,
) -> Result<String> {
    git::add_remote(dir, "origin", url)?;
    // A remote that can't be fetched is not kept, so a retry with the URL
    // fixed starts from where this one did.
    if let Err(e) = git::fetch_remote(dir, "origin") {
        let _ = git::remove_remote(dir, "origin");
        return Err(PmError::Git(format!(
            "could not fetch {label} from {url}, so nothing was changed: {e}"
        )));
    }

    if !fresh {
        // Existing repo connecting to a remote for the first time: local
        // and remote have independent root commits, so ff-only pull can't
        // work.
        let taken = take_remote(dir, label)?;
        return Ok(format!("Set {label} remote to {url}; {taken}"));
    }
    match remote_branch(dir)? {
        None => Ok(format!("Set {label} remote to {url} (remote is empty)")),
        Some(remote_ref) => {
            let local_branch = remote_ref.strip_prefix("origin/").unwrap_or(&remote_ref);
            git::reset_to_remote_branch(dir, local_branch, &remote_ref)?;
            Ok(format!("Set {label} remote to {url} and pulled"))
        }
    }
}

/// The fetched remote branch to work on: `origin/main`, else
/// `origin/master`, else the first; `None` when the remote is empty.
pub(super) fn remote_branch(dir: &Path) -> Result<Option<String>> {
    let remote_branches = git::list_remote_branches(dir)?;
    Ok(remote_branches
        .iter()
        .find(|b| *b == "origin/main")
        .or_else(|| remote_branches.iter().find(|b| *b == "origin/master"))
        .or_else(|| remote_branches.first())
        .cloned())
}

/// Replace the repo's content with its fetched remote's, on a local branch
/// named after the remote's and tracking it, so a later `push` updates that
/// branch rather than creating a second one. Dirty state is committed
/// first, so it stays in the reflog.
pub(super) fn take_remote(dir: &Path, label: &str) -> Result<String> {
    let ctx = RepoContext {
        dir,
        label,
        init_hint: "",
        remote_hint: "",
    };
    commit_if_dirty(&ctx)?;
    git::fetch_remote(dir, "origin")?;
    let Some(remote_ref) = remote_branch(dir)? else {
        return Ok("the remote is empty".to_string());
    };
    eprintln!(
        "warning: resetting {label} to remote — local state is overwritten \
         (previous commits are preserved in git reflog)"
    );
    let local_branch = remote_ref.strip_prefix("origin/").unwrap_or(&remote_ref);
    git::reset_to_remote_branch(dir, local_branch, &remote_ref)?;
    Ok("took the remote's content".to_string())
}

/// Remote setup choices.
enum RemoteChoice {
    GitHub,
    Url(String),
    Skip,
}

/// Read the user's remote setup choice from stdin.
fn read_remote_choice() -> Result<RemoteChoice> {
    // When stdin is not a terminal (e.g. tests, piped input, closed fd),
    // skip the interactive prompt entirely to avoid blocking.
    if !io::stdin().is_terminal() {
        return Ok(RemoteChoice::Skip);
    }

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
pub(super) fn prompt_remote_setup_common(
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
