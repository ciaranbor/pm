//! Initialising a state repo, optionally from a remote, shared by both
//! repos.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;

use super::remote::{apply_remote_and_pull, take_remote};
use super::repo::{RepoContext, pull_repo};

/// Closure that runs before the first commit (e.g. to write a .gitignore).
type PreInitHook<'a> = Box<dyn FnOnce(&Path) -> Result<()> + 'a>;

/// Closure that runs after a remote URL is successfully configured.
type PostRemoteHook<'a> = Box<dyn FnOnce() -> Result<()> + 'a>;

/// Closure that interactively prompts for remote setup.
type PromptRemoteHook<'a> = Box<dyn FnOnce(&Path) -> Result<Option<String>> + 'a>;

/// Configuration for `init_repo_managed`, capturing the differences between
/// project-level and global-registry init.
pub(super) struct InitConfig<'a> {
    /// Directory to initialise as a git repo.
    pub(super) dir: &'a Path,
    /// Human-readable label (e.g. "state", "global registry").
    pub(super) label: &'a str,
    /// Error message when the directory does not exist.
    pub(super) dir_missing_error: &'a str,
    /// Commit message for the initial commit.
    pub(super) init_commit_msg: &'a str,
    /// Message returned on successful init (e.g. "Initialised state repo in .pm/").
    pub(super) init_success_msg: String,
    /// Message returned when the repo already exists.
    pub(super) already_init_msg: &'a str,
    /// The command that pulls this repo, for the hint when `--remote`
    /// names a different remote than the one it has.
    pub(super) pull_hint: &'a str,
    /// Whether `--remote` naming the repo's own remote takes the remote's
    /// content when a fast-forward can't, instead of refusing.
    pub(super) reset_when_diverged: bool,
    /// Called before the first commit (e.g. to write a .gitignore).
    pub(super) pre_init: Option<PreInitHook<'a>>,
    /// Called after a remote URL is successfully configured.
    pub(super) post_remote: Option<PostRemoteHook<'a>>,
    /// Called to interactively prompt for remote setup.
    pub(super) prompt_remote: Option<PromptRemoteHook<'a>>,
}

/// Unified init logic for both project-level and global-registry state repos.
/// Returns the report, and whether the repo was reset to its remote's
/// content (rather than fast-forwarded or left alone).
pub(super) fn init_repo_managed(
    cfg: InitConfig,
    interactive: bool,
    remote_url: Option<&str>,
) -> Result<(String, bool)> {
    let dir = cfg.dir;
    let label = cfg.label;

    if dir.join(".git").exists() {
        // Already initialised — if --remote given, configure it if possible
        if let Some(url) = remote_url {
            if let Some(current) = git::remote_url(dir, "origin")? {
                if current != url {
                    return Err(PmError::Git(format!(
                        "{label} repo already has remote {current}; to use {url} instead, run \
                         `git -C {} remote set-url origin {url}`, then `{}`",
                        crate::tmux::shell_quote(&dir.to_string_lossy()),
                        cfg.pull_hint
                    )));
                }
                let ctx = RepoContext {
                    dir,
                    label,
                    init_hint: "",
                    remote_hint: "",
                };
                let header = format!("{} with remote {url}", cfg.already_init_msg);
                return match pull_repo(&ctx) {
                    Ok(pulled) => Ok((format!("{header}\n{pulled}"), false)),
                    // A fast-forward can't work: local and remote both have
                    // commits, or share none (a repo `pm register` made
                    // here before the first pull).
                    Err(_) if cfg.reset_when_diverged => {
                        let taken = take_remote(dir, label)?;
                        Ok((format!("{header}\n{taken}"), true))
                    }
                    Err(e) => Err(PmError::Git(format!(
                        "{e}\n{label} repo and {url} have both changed; reconcile them with \
                         `git -C {} pull --no-rebase origin`, then `{}` again",
                        crate::tmux::shell_quote(&dir.to_string_lossy()),
                        cfg.pull_hint
                    ))),
                };
            }
            let mut result = cfg.already_init_msg.to_string();
            let remote_msg = apply_remote_and_pull(dir, url, label, false)?;
            result.push('\n');
            result.push_str(&remote_msg);
            if let Some(post) = cfg.post_remote
                && let Err(e) = post()
            {
                eprintln!("warning: {label} post-remote hook failed: {e}");
            }
            return Ok((result, true));
        }
        // If interactive and no remote, offer remote setup
        if interactive && !git::has_remote(dir, "origin")? {
            let mut result = cfg.already_init_msg.to_string();
            if let Some(prompt_fn) = cfg.prompt_remote
                && let Some(remote_msg) = prompt_fn(dir)?
            {
                result.push('\n');
                result.push_str(&remote_msg);
            }
            return Ok((result, false));
        }
        return Ok((cfg.already_init_msg.to_string(), false));
    }

    if !dir.exists() {
        return Err(PmError::Git(cfg.dir_missing_error.to_string()));
    }

    let before = entry_names(dir)?;

    // Run pre-init hook (e.g. write .gitignore for global registry)
    if let Some(pre) = cfg.pre_init {
        pre(dir)?;
    }

    // Init the repo (creates initial empty commit)
    git::init_repo(dir)?;

    // When --remote is given, skip committing local state — the remote is
    // authoritative and apply_remote_and_pull will fetch + reset to it.
    if remote_url.is_none() {
        git::add_all(dir)?;
        if git::has_staged_changes(dir)? {
            git::commit_with_message(dir, cfg.init_commit_msg)?;
        }
    }

    let mut result = cfg.init_success_msg;

    // Explicit remote URL takes precedence over interactive prompt
    if let Some(url) = remote_url {
        let remote_msg = match apply_remote_and_pull(dir, url, label, true) {
            Ok(msg) => msg,
            Err(e) => {
                remove_entries_since(dir, &before);
                return Err(e);
            }
        };
        result.push('\n');
        result.push_str(&remote_msg);
        if let Some(post) = cfg.post_remote
            && let Err(e) = post()
        {
            eprintln!("warning: {label} post-remote hook failed: {e}");
        }
    } else if interactive
        && let Some(prompt_fn) = cfg.prompt_remote
        && let Some(remote_msg) = prompt_fn(dir)?
    {
        result.push('\n');
        result.push_str(&remote_msg);
    }

    Ok((result, remote_url.is_some()))
}

/// The names of the entries of `dir`.
fn entry_names(dir: &Path) -> Result<Vec<std::ffi::OsString>> {
    Ok(std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.file_name())
        .collect())
}

/// Undo a failed init: remove whatever it created in `dir`, the repo
/// included, leaving the entries named in `before`.
fn remove_entries_since(dir: &Path, before: &[std::ffi::OsString]) {
    for name in entry_names(dir).unwrap_or_default() {
        if before.contains(&name) {
            continue;
        }
        let path = dir.join(&name);
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}
