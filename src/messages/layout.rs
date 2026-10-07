//! Where an inbox lives on disk.
//!
//! `<messages>/<feature>/<agent>/` holds one `from-<sender>/` queue of
//! numbered `NNN.md` bodies per sender, their metadata under
//! `.meta/from-<sender>/NNN.json`, and the `.cursor` and `.last_read` files.
//! A queue's latest index is its highest-numbered body.

use std::path::{Path, PathBuf};

use crate::error::Result;

/// Returns the inbox directory for an agent in a feature.
pub(super) fn inbox_dir(messages_dir: &Path, feature: &str, agent: &str) -> PathBuf {
    messages_dir.join(feature).join(agent)
}

/// Returns the sender subdirectory within an agent's inbox.
pub(super) fn sender_dir(messages_dir: &Path, feature: &str, agent: &str, sender: &str) -> PathBuf {
    inbox_dir(messages_dir, feature, agent).join(format!("from-{sender}"))
}

/// Returns the meta directory for a sender within an agent's inbox.
pub(super) fn meta_dir(messages_dir: &Path, feature: &str, agent: &str, sender: &str) -> PathBuf {
    inbox_dir(messages_dir, feature, agent)
        .join(".meta")
        .join(format!("from-{sender}"))
}

/// Find the highest message index in a directory of numbered .md files.
/// Returns 0 if the directory is empty or doesn't exist.
pub(super) fn max_index(dir: &Path) -> Result<u32> {
    if !dir.exists() {
        return Ok(0);
    }

    let mut max: u32 = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_suffix(".md")
            && let Ok(n) = stem.parse::<u32>()
        {
            max = max.max(n);
        }
    }
    Ok(max)
}

/// List all senders who have sent messages to an inbox.
pub(super) fn list_senders(inbox: &Path) -> Result<Vec<String>> {
    let mut senders = Vec::new();
    if !inbox.exists() {
        return Ok(senders);
    }
    for entry in std::fs::read_dir(inbox)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(sender) = name.strip_prefix("from-")
            && entry.path().is_dir()
        {
            senders.push(sender.to_string());
        }
    }
    senders.sort();
    Ok(senders)
}
