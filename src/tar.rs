//! gzip-compressed tar archives, made and unpacked by the system `tar`.

use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};

/// Write `entry`, a path relative to `dir`, to the archive `archive`.
pub fn create(archive: &Path, dir: &Path, entry: &str) -> Result<()> {
    run(Command::new("tar")
        .arg("-czf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .arg(entry))
}

/// Unpack the archive `archive` into `dir`.
pub fn extract(archive: &Path, dir: &Path) -> Result<()> {
    run(Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dir))
}

fn run(command: &mut Command) -> Result<()> {
    let output = command
        .output()
        .map_err(|e| PmError::Tar(format!("could not run tar: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PmError::Tar(format!(
            "tar failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_extract_reports_what_tar_said() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("not-an-archive.tar.gz");
        std::fs::write(&archive, "plain text").unwrap();

        let Err(PmError::Tar(message)) = extract(&archive, dir.path()) else {
            panic!("extracting a non-archive succeeded");
        };
        let said = message
            .split_once("): ")
            .map(|(_, said)| said)
            .unwrap_or("");
        assert!(!said.is_empty(), "{message}");
    }
}
