//! Operations common to both state repos: setting the remote, push, pull,
//! status.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;

/// Context for operating on a git-backed state directory.
///
/// Both project-level (`.pm/`) and global (`~/.config/pm/`) state repos
/// use the same logic — only labels and hint messages differ.
pub(super) struct RepoContext<'a> {
    pub(super) dir: &'a Path,
    pub(super) label: &'a str,
    pub(super) init_hint: &'a str,
    pub(super) remote_hint: &'a str,
}

/// Verify the directory has a git repo.
pub(super) fn require_repo(ctx: &RepoContext) -> Result<()> {
    if !ctx.dir.join(".git").exists() {
        return Err(PmError::Git(format!(
            "{} repo not initialised (run `{}`)",
            ctx.label, ctx.init_hint
        )));
    }
    Ok(())
}

/// Set the remote URL for a state repo.
pub(super) fn set_remote(ctx: &RepoContext, url: &str) -> Result<String> {
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

/// Auto-commit and (if a remote is configured) push a state repo.
///
/// A missing remote is not an error: remote sync is opt-in. Without one,
/// local changes are still committed and the function exits successfully
/// with an informational note.
pub(super) fn push_repo(ctx: &RepoContext) -> Result<String> {
    require_repo(ctx)?;

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

    if !git::has_remote(ctx.dir, "origin")? {
        return Ok(if committed {
            format!(
                "Committed {} locally; no remote configured (set one with `{}`)",
                ctx.label, ctx.remote_hint
            )
        } else {
            format!(
                "{} has no new changes; no remote configured (set one with `{}`)",
                capitalize(ctx.label),
                ctx.remote_hint
            )
        });
    }

    let branch = git::current_branch(ctx.dir)?;
    git::push(ctx.dir, "origin", &branch)?;

    if committed {
        Ok(format!("Committed and pushed {}", ctx.label))
    } else {
        Ok(format!("Pushed {} (no new changes to commit)", ctx.label))
    }
}

/// Pull state from the remote, auto-committing dirty state first.
///
/// A missing remote is not an error: with no remote there's nothing to
/// pull, so this is an informational no-op.
pub(super) fn pull_repo(ctx: &RepoContext) -> Result<String> {
    require_repo(ctx)?;

    if !git::has_remote(ctx.dir, "origin")? {
        return Ok(format!(
            "No remote configured for {}; nothing to pull (set one with `{}`)",
            ctx.label, ctx.remote_hint
        ));
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

/// Show git status of a state repo. A missing remote is noted as plain
/// info, not treated as an error.
pub(super) fn status_repo(ctx: &RepoContext) -> Result<String> {
    require_repo(ctx)?;
    let output = git::status_short(ctx.dir)?;
    let mut result = if output.is_empty() {
        format!("{} repo is clean", capitalize(ctx.label))
    } else {
        output
    };
    if !git::has_remote(ctx.dir, "origin")? {
        result.push_str(&format!(
            "\n(no remote configured — set one with `{}`)",
            ctx.remote_hint
        ));
    }
    Ok(result)
}

/// Stage all changes and commit if there's anything to commit.
pub(super) fn commit_if_dirty(ctx: &RepoContext) -> Result<()> {
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
