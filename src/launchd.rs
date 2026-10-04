//! `launchctl`, which loads and unloads the user's LaunchAgents.

use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};

/// The user's GUI login domain, where LaunchAgents run.
fn domain() -> String {
    // SAFETY: getuid cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

/// `launchctl` acts on the user's real login domain, so a test build
/// refuses to run it.
fn command() -> Command {
    if cfg!(test) {
        panic!("tests must not run launchctl");
    }
    Command::new("launchctl")
}

fn launchctl(args: &[&str]) -> Result<()> {
    let output = command().args(args).output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(PmError::Serve(format!(
        "launchctl {} failed: {stderr}",
        args.join(" ")
    )))
}

/// Load and start the agent defined by `plist`.
pub fn bootstrap(plist: &Path) -> Result<()> {
    launchctl(&["bootstrap", &domain(), &plist.to_string_lossy()])
}

fn loaded(target: &str) -> Result<bool> {
    Ok(command().args(["print", target]).output()?.status.success())
}

/// A loaded agent `label`: its pid while it runs. `None` when not loaded.
pub fn job(label: &str) -> Result<Option<Option<u32>>> {
    let output = command()
        .args(["print", &format!("{}/{label}", domain())])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|l| l.trim().strip_prefix("pid = ")?.parse().ok()),
    ))
}

/// Stop and unload the agent `label`; one not loaded is left as it is.
/// `bootout` may return while the job is still going, and a `bootstrap` of
/// the same label then fails, so this waits a few seconds for it to go.
pub fn bootout(label: &str) -> Result<()> {
    let target = format!("{}/{label}", domain());
    if !loaded(&target)? {
        return Ok(());
    }
    launchctl(&["bootout", &target])?;
    for _ in 0..50 {
        if !loaded(&target)? {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(PmError::Serve(format!(
        "{label} is still loaded after bootout"
    )))
}
