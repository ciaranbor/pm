//! The files the plugin reports through, in the agent's runtime dir: why
//! its loop stopped, its last turn's error, and that its setup ran.

use std::path::{Path, PathBuf};

use crate::error::Result;

/// A per-spawn file in the agent's runtime dir.
pub(super) fn spawn_file(
    project_root: &Path,
    scope: &str,
    agent: &str,
    extension: &str,
) -> Result<PathBuf> {
    Ok(
        crate::state::runtime::agent_dir(project_root, scope, agent)?
            .join(format!("opencode.{extension}")),
    )
}

/// Where the plugin records why its loop stopped. The plugin writes the
/// other status files beside it under the names below.
pub(in crate::harness) fn trip_file(
    project_root: &Path,
    scope: &str,
    agent: &str,
) -> Result<PathBuf> {
    spawn_file(project_root, scope, agent, "tripped")
}

/// Written by the plugin once its setup has run in this spawn's TUI.
pub(in crate::harness) fn loaded_file(
    project_root: &Path,
    scope: &str,
    agent: &str,
) -> Result<PathBuf> {
    spawn_file(project_root, scope, agent, "loaded")
}

/// The error of the agent's last turn, while that turn is the last to have
/// failed or succeeded.
pub(in crate::harness) fn turn_error_file(
    project_root: &Path,
    scope: &str,
    agent: &str,
) -> Result<PathBuf> {
    spawn_file(project_root, scope, agent, "turn-error")
}

/// Remove what the plugin reported about an earlier spawn of the agent.
pub(super) fn clear_status_files(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    for file in [
        trip_file(project_root, scope, agent)?,
        loaded_file(project_root, scope, agent)?,
        turn_error_file(project_root, scope, agent)?,
    ] {
        match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_features_agents_of_one_name_get_a_config_file_each() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_ne!(
            spawn_file(root, "login", "reviewer", "json").unwrap(),
            spawn_file(root, "signup", "reviewer", "json").unwrap()
        );
    }
}
