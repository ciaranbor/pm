//! Per-agent runtime files: what a spawn hands its harness (the composed
//! prompt, opencode's config) and what the harness leaves for pm (opencode's
//! stopped-loop marker).
//!
//! They live in `<project>/.pm/runtime/<scope>/<agent>/` and last as long
//! as the agent's registry entry. Every spawn rewrites what it hands the
//! harness, so a deleted file is restored by the next spawn. The directory
//! ignores itself, keeping machine-specific files out of the `.pm/` state
//! repo.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::state::paths;

fn root(project_root: &Path) -> PathBuf {
    paths::pm_dir(project_root).join("runtime")
}

fn scope_dir(project_root: &Path, scope: &str) -> PathBuf {
    root(project_root).join(scope)
}

/// The agent's runtime dir, created if missing.
pub fn agent_dir(project_root: &Path, scope: &str, agent: &str) -> Result<PathBuf> {
    let dir = scope_dir(project_root, scope).join(agent);
    std::fs::create_dir_all(&dir)?;
    let ignore = root(project_root).join(".gitignore");
    if !ignore.exists() {
        std::fs::write(ignore, "*\n")?;
    }
    Ok(dir)
}

/// Remove one agent's runtime files.
pub fn remove_agent(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    remove(&scope_dir(project_root, scope).join(agent))
}

/// Remove the runtime files of every agent in a scope.
pub fn remove_scope(project_root: &Path, scope: &str) -> Result<()> {
    remove(&scope_dir(project_root, scope))
}

fn remove(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn runtime_files_stay_out_of_the_state_repo() {
        let dir = tempdir().unwrap();
        let pm_dir = paths::pm_dir(dir.path());
        std::fs::create_dir_all(&pm_dir).unwrap();
        crate::git::run_git(&pm_dir, &["init"]).unwrap();

        let agent = agent_dir(dir.path(), "login", "reviewer").unwrap();
        std::fs::write(agent.join("prompt.md"), "x").unwrap();

        let status =
            crate::git::run_git(&pm_dir, &["status", "--porcelain", "--untracked-files=all"])
                .unwrap();
        assert_eq!(status, "");
    }

    #[test]
    fn removal_is_scoped_to_the_agent_or_scope() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let reviewer = agent_dir(root, "login", "reviewer").unwrap();
        let implementer = agent_dir(root, "login", "implementer").unwrap();
        let other = agent_dir(root, "signup", "reviewer").unwrap();

        remove_agent(root, "login", "reviewer").unwrap();
        assert!(!reviewer.exists());
        assert!(implementer.exists());

        remove_scope(root, "login").unwrap();
        assert!(!implementer.exists());
        assert!(other.exists());

        // Removing what is already gone is not an error.
        remove_scope(root, "login").unwrap();
    }
}
