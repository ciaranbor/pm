mod branch;
mod init;
mod remote;
pub(crate) mod status;
mod worktree;

pub use branch::*;
pub use init::*;
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
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(PmError::Git(stderr))
    }
}
