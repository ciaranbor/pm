//! The global registry repo's `.gitignore`.
//!
//! `pm state init --global` creates it with the machine-specific ephemera
//! the registry may accumulate. Releases that installed the bundled
//! workflows into the config dir also wrote a block naming them, and pm
//! leaves that block in place: a machine sharing the registry may still run
//! such a release, and without the block its next push would commit its
//! bundled copies to every machine.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::fs_utils::write_atomic;

/// The `.gitignore` a new registry repo starts from.
const BASE: &str = "\
# Ephemeral / machine-specific state
*.lock
*.pid
";

fn gitignore_path(config_dir: &Path) -> PathBuf {
    config_dir.join(".gitignore")
}

/// Write the registry `.gitignore` when absent.
pub fn write_global_gitignore(config_dir: &Path) -> Result<()> {
    let path = gitignore_path(config_dir);
    if !path.exists() {
        write_atomic(&path, BASE.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_creates_the_base_once_and_keeps_user_edits() {
        let dir = tempdir().unwrap();
        write_global_gitignore(dir.path()).unwrap();
        let file = dir.path().join(".gitignore");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), BASE);
        std::fs::write(&file, "mine\n").unwrap();
        write_global_gitignore(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine\n");
    }
}
