//! Listing every message in an inbox with its status against the cursor.

use std::path::Path;

use chrono::Utc;

use super::address::{matches, validate_key};
use super::cursor;
use super::layout::{inbox_dir, list_senders, max_index, sender_dir};
use super::read::load_meta;
use super::types::{MessageStatus, MessageSummary};
use crate::error::Result;

/// Enumerate all messages in an agent's inbox (optionally only the senders
/// `from` [matches](super::address::matches)) along with their status
/// relative to the cursor. Returns a list grouped by sender, then by index
/// ascending. Does not touch the cursor.
pub fn list(
    messages_dir: &Path,
    feature: &str,
    agent: &str,
    from: Option<&str>,
) -> Result<Vec<MessageSummary>> {
    let inbox = inbox_dir(messages_dir, feature, agent);
    if !inbox.exists() {
        return Ok(Vec::new());
    }

    let csr = cursor::load_cursor(&cursor::cursor_path(messages_dir, feature, agent))?;
    if let Some(from) = from {
        validate_key(from)?;
    }
    let senders: Vec<String> = list_senders(&inbox)?
        .into_iter()
        .filter(|key| from.is_none_or(|from| matches(key, from)))
        .collect();

    let mut out = Vec::new();
    for sender in &senders {
        let sdir = sender_dir(messages_dir, feature, agent, sender);
        if !sdir.exists() {
            continue;
        }
        let latest = max_index(&sdir)?;
        let cur = csr.get(sender.as_str()).copied().unwrap_or(0);

        for i in 1..=latest {
            let msg_path = sdir.join(format!("{i:03}.md"));
            if !msg_path.exists() {
                continue;
            }
            let body = std::fs::read_to_string(&msg_path)?;
            let first_line = body
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect::<String>();

            let timestamp = load_meta(messages_dir, feature, agent, sender, i)?
                .map_or_else(Utc::now, |m| m.timestamp);

            let status = if i <= cur {
                MessageStatus::Read
            } else if i == cur + 1 {
                MessageStatus::Next
            } else {
                MessageStatus::Queued
            };

            out.push(MessageSummary {
                sender: sender.clone(),
                index: i,
                timestamp,
                first_line,
                status,
            });
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::test_support::messages_dir;
    use crate::messages::{next, send};
    use tempfile::tempdir;

    #[test]
    fn list_empty_inbox() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let v = list(&mdir, "login", "reviewer", None).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn list_marks_cursor_position() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        send(&mdir, "login", "reviewer", "implementer", "two").unwrap();
        send(&mdir, "login", "reviewer", "implementer", "three").unwrap();

        // Cursor at 0: message 1 is "next", 2 and 3 are "queued".
        let v = list(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].status, MessageStatus::Next);
        assert_eq!(v[1].status, MessageStatus::Queued);
        assert_eq!(v[2].status, MessageStatus::Queued);

        // After advancing, cursor is 1: 1 is "read", 2 is "next", 3 is "queued".
        next(&mdir, "login", "reviewer", "implementer@login").unwrap();
        let v = list(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(v[0].status, MessageStatus::Read);
        assert_eq!(v[1].status, MessageStatus::Next);
        assert_eq!(v[2].status, MessageStatus::Queued);
    }

    #[test]
    fn list_groups_by_sender() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "impl-1").unwrap();
        send(&mdir, "login", "reviewer", "user", "user-1").unwrap();
        send(&mdir, "login", "reviewer", "implementer", "impl-2").unwrap();

        let v = list(&mdir, "login", "reviewer", None).unwrap();
        // Sender-grouped, sorted alphabetically: implementer then user.
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].sender, "implementer@login");
        assert_eq!(v[0].index, 1);
        assert_eq!(v[1].sender, "implementer@login");
        assert_eq!(v[1].index, 2);
        assert_eq!(v[2].sender, "user@login");
        assert_eq!(v[2].index, 1);
    }

    #[test]
    fn list_scoped_to_one_sender() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "impl").unwrap();
        send(&mdir, "login", "reviewer", "user", "user").unwrap();

        let v = list(&mdir, "login", "reviewer", Some("user@login")).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].sender, "user@login");
    }

    #[test]
    fn list_records_first_line_preview() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(
            &mdir,
            "login",
            "reviewer",
            "implementer",
            "this is the first line\nand a second line\n",
        )
        .unwrap();

        let v = list(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(v[0].first_line, "this is the first line");
    }
}
