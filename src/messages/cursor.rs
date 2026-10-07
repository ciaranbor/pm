//! The per-sender read cursor: the last index each sender's queue has been
//! read to.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};

use super::types::Cursor;
use super::validation::validate_name;

/// Returns the cursor file path for an agent's inbox.
pub(crate) fn cursor_path(messages_dir: &Path, feature: &str, agent: &str) -> PathBuf {
    super::layout::inbox_dir(messages_dir, feature, agent).join(".cursor")
}

pub(crate) fn load_cursor(path: &Path) -> Result<Cursor> {
    if !path.exists() {
        return Ok(Cursor::new());
    }
    let content = std::fs::read_to_string(path)?;
    let cursor: Cursor = serde_json::from_str(&content)?;
    Ok(cursor)
}

pub(crate) fn save_cursor(path: &Path, cursor: &Cursor) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(cursor)?;
    let tmp = path.with_extension("cursor.tmp");
    std::fs::write(&tmp, &content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Read the cursor position (last-processed index) for one sender in an
/// agent's inbox. Returns 0 if the sender has never sent a message or the
/// cursor has not been advanced.
pub fn cursor_for(messages_dir: &Path, feature: &str, agent: &str, sender: &str) -> Result<u32> {
    let cursor = load_cursor(&cursor_path(messages_dir, feature, agent))?;
    Ok(cursor.get(sender).copied().unwrap_or(0))
}

/// Advance the cursor for one sender by exactly one position. Returns the
/// new cursor position. Errors if the cursor is already at the latest
/// index (nothing to advance past).
pub fn next(messages_dir: &Path, feature: &str, agent: &str, sender: &str) -> Result<u32> {
    validate_name(feature, "feature")?;
    validate_name(agent, "agent")?;
    validate_name(sender, "sender")?;

    let sdir = super::layout::sender_dir(messages_dir, feature, agent, sender);
    let latest = super::layout::max_index(&sdir)?;

    let cpath = cursor_path(messages_dir, feature, agent);
    let mut cursor = load_cursor(&cpath)?;
    let current = cursor.get(sender).copied().unwrap_or(0);

    if current >= latest {
        return Err(PmError::Messaging(format!(
            "no messages to advance past from {sender} (cursor at {current}, latest is {latest})"
        )));
    }

    let new = current + 1;
    cursor.insert(sender.to_string(), new);
    save_cursor(&cpath, &cursor)?;
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PmError;
    use crate::messages::send;
    use crate::messages::test_support::messages_dir;
    use tempfile::tempdir;

    #[test]
    fn next_advances_cursor_by_one() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        send(&mdir, "login", "reviewer", "implementer", "two").unwrap();
        send(&mdir, "login", "reviewer", "implementer", "three").unwrap();

        assert_eq!(
            cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            0
        );
        assert_eq!(next(&mdir, "login", "reviewer", "implementer").unwrap(), 1);
        assert_eq!(
            cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            1
        );
        assert_eq!(next(&mdir, "login", "reviewer", "implementer").unwrap(), 2);
        assert_eq!(next(&mdir, "login", "reviewer", "implementer").unwrap(), 3);
        assert_eq!(
            cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            3
        );
    }

    #[test]
    fn next_errors_at_latest() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        next(&mdir, "login", "reviewer", "implementer").unwrap();

        let err = next(&mdir, "login", "reviewer", "implementer").unwrap_err();
        assert!(matches!(err, PmError::Messaging(_)));
        assert!(format!("{err}").contains("no messages to advance past"));
    }

    #[test]
    fn next_errors_on_empty_sender() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        // No messages from "implementer" yet.
        let err = next(&mdir, "login", "reviewer", "implementer").unwrap_err();
        assert!(matches!(err, PmError::Messaging(_)));
    }

    #[test]
    fn next_only_advances_named_sender() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "impl").unwrap();
        send(&mdir, "login", "reviewer", "user", "user").unwrap();

        next(&mdir, "login", "reviewer", "implementer").unwrap();

        assert_eq!(
            cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            1
        );
        assert_eq!(cursor_for(&mdir, "login", "reviewer", "user").unwrap(), 0);
    }
}
