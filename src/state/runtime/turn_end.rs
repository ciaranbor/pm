//! A turn's end read from the transcript, for a harness that records one
//! only there, has no marker to remove, so the one who acts on it claims it
//! with a stamp instead, after which it no longer counts.

use std::path::{Path, PathBuf};

use super::{agent_dir, agent_file};
use crate::error::Result;

const TURN_END_CLAIM: &str = "turn-end-claimed-";

/// The file whose existence says the turn end recorded by transcript entry
/// `entry` was claimed.
fn turn_end_claim(project_root: &Path, scope: &str, agent: &str, entry: &str) -> PathBuf {
    let name: String = entry
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    agent_file(
        project_root,
        scope,
        agent,
        &format!("{TURN_END_CLAIM}{name}"),
    )
}

/// Claim the turn end transcript entry `entry` records, one no hook
/// reported, for one caller only: true for the caller that claimed it.
/// Earlier claims are dropped. Keyed on the entry, not the transcript's
/// mtime, so bookkeeping written after it leaves the claim standing.
pub fn claim_turn_end(project_root: &Path, scope: &str, agent: &str, entry: &str) -> Result<bool> {
    let dir = agent_dir(project_root, scope, agent)?;
    let file = turn_end_claim(project_root, scope, agent, entry);
    match std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(&file)
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e.into()),
    }
    for dirent in std::fs::read_dir(dir)?.flatten() {
        let path = dirent.path();
        let stale = dirent
            .file_name()
            .to_string_lossy()
            .starts_with(TURN_END_CLAIM);
        if stale && path != file {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(true)
}

/// Give up a claim [`claim_turn_end`] made.
pub fn release_turn_end(project_root: &Path, scope: &str, agent: &str, entry: &str) -> Result<()> {
    match std::fs::remove_file(turn_end_claim(project_root, scope, agent, entry)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Whether the turn end transcript entry `entry` records has been claimed.
pub fn turn_end_claimed(project_root: &Path, scope: &str, agent: &str, entry: &str) -> bool {
    turn_end_claim(project_root, scope, agent, entry).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_turn_end_is_claimed_once_until_given_back() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let first = "6f1c2a8e-0001";
        let later = "6f1c2a8e-0002";

        assert!(claim_turn_end(root, "login", "qa", first).unwrap());
        assert!(!claim_turn_end(root, "login", "qa", first).unwrap());
        assert!(turn_end_claimed(root, "login", "qa", first));

        assert!(claim_turn_end(root, "login", "qa", later).unwrap());
        release_turn_end(root, "login", "qa", later).unwrap();
        assert!(!turn_end_claimed(root, "login", "qa", later));
        assert!(claim_turn_end(root, "login", "qa", later).unwrap());
    }
}
