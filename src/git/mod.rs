mod branch;
mod init;
mod rebase;
mod remote;
pub(crate) mod status;
mod worktree;

pub use branch::*;
pub use init::*;
pub use rebase::*;
pub use remote::*;
pub use status::*;
pub use worktree::*;

#[cfg(test)]
pub(crate) use init::init_bare;
#[cfg(test)]
pub(crate) use status::{cat_file, commit, stage_file};

use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};

pub(crate) fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", &repo.to_string_lossy()])
        .args(args)
        .output()?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(failure(&output))
    }
}

/// The error of a failed git command, with what it said: some explain a
/// failure on stdout alone (`git merge`'s conflicts), so both streams are
/// kept.
fn failure(output: &std::process::Output) -> PmError {
    PmError::Git(
        [&output.stderr, &output.stdout]
            .iter()
            .map(|s| String::from_utf8_lossy(s).trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}
