//! The global registry repo: present, with a remote, clean, and pushed.

use std::path::Path;

use crate::error::Result;
use crate::git;

use super::line::Line;
use super::repo::{self, Remote};
use super::{Finding, Step, shell_path};

pub(super) fn findings(config_dir: &Path) -> Result<Vec<Finding>> {
    let mut line = Line::default();
    if !git::is_git_repo(config_dir) {
        line.problem("not a git repo, so the new host can't pull it");
        line.steps(&[Step::RegistryInit, Step::Backfill, Step::GlobalPush]);
        return Ok(line.finish("").into_iter().collect());
    }
    let remote = Remote::of(config_dir)?;
    if remote.is_none() {
        line.problem("no remote");
        line.steps(&[Step::RegistryRemote, Step::GlobalPush]);
    }
    let note = repo::state_repo(
        &mut line,
        config_dir,
        &shell_path(config_dir),
        "",
        remote.as_ref(),
        Step::GlobalPush,
        "pm state pull --global",
    )?;
    Ok(line.finish("").into_iter().chain(note).collect())
}
