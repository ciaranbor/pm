//! Reading an inbox without moving its cursor: unread counts and single
//! messages by index.

use std::path::Path;

use chrono::Utc;

use super::cursor;
use super::layout::{inbox_dir, max_index, meta_dir, sender_dir};
use super::types::{Message, MessageMeta, UnreadSummary};
use super::validation::validate_name;
use crate::error::Result;

/// The number of unread messages in an agent's inbox; 0 when it cannot be
/// read.
pub fn unread_count(messages_dir: &Path, feature: &str, agent: &str) -> u32 {
    check(messages_dir, feature, agent)
        .map(|senders| senders.iter().map(|s| s.count).sum())
        .unwrap_or(0)
}

/// How many messages the agent has read, over every sender; 0 when its
/// inbox cannot be read. It only grows, so a change means a read.
pub fn read_count(messages_dir: &Path, feature: &str, agent: &str) -> u32 {
    cursor::load_cursor(&cursor::cursor_path(messages_dir, feature, agent))
        .map(|cursor| cursor.values().sum())
        .unwrap_or(0)
}

/// Check for unread messages in an agent's inbox. Returns unread counts per sender.
pub fn check(messages_dir: &Path, feature: &str, agent: &str) -> Result<Vec<UnreadSummary>> {
    let inbox = inbox_dir(messages_dir, feature, agent);
    if !inbox.exists() {
        return Ok(Vec::new());
    }

    let csr = cursor::load_cursor(&cursor::cursor_path(messages_dir, feature, agent))?;
    let mut summaries = Vec::new();

    for entry in std::fs::read_dir(&inbox)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if let Some(sender) = name.strip_prefix("from-") {
            if !entry.path().is_dir() {
                continue;
            }
            let last_read = csr.get(sender).copied().unwrap_or(0);
            let latest = max_index(&entry.path())?;
            if latest > last_read {
                summaries.push(UnreadSummary {
                    sender: sender.to_string(),
                    count: latest - last_read,
                });
            }
        }
    }

    summaries.sort_by(|a, b| a.sender.cmp(&b.sender));
    Ok(summaries)
}

/// Load the metadata recorded for one message, `None` when the message
/// predates metadata.
pub(super) fn load_meta(
    messages_dir: &Path,
    feature: &str,
    agent: &str,
    sender: &str,
    index: u32,
) -> Result<Option<MessageMeta>> {
    let meta_path = meta_dir(messages_dir, feature, agent, sender).join(format!("{index:03}.json"));
    if !meta_path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&meta_path)?;
    Ok(Some(serde_json::from_str(&content)?))
}

/// Load a single message at an absolute index from a specific sender. Pure
/// read: does not touch the cursor. Returns `Ok(None)` if the index does
/// not refer to an existing message file (out of range, never sent, or the
/// inbox does not exist at all).
pub fn read_at(
    messages_dir: &Path,
    feature: &str,
    agent: &str,
    sender: &str,
    index: u32,
) -> Result<Option<Message>> {
    validate_name(feature, "feature")?;
    validate_name(agent, "agent")?;
    validate_name(sender, "sender")?;

    if index == 0 {
        return Ok(None);
    }

    let sdir = sender_dir(messages_dir, feature, agent, sender);
    let msg_path = sdir.join(format!("{index:03}.md"));
    if !msg_path.exists() {
        return Ok(None);
    }

    let body = std::fs::read_to_string(&msg_path)?;
    let meta = load_meta(messages_dir, feature, agent, sender, index)?.unwrap_or(MessageMeta {
        sender: sender.to_string(),
        timestamp: Utc::now(),
        sender_scope: None,
        sender_project: None,
    });

    Ok(Some(Message {
        index,
        sender: sender.to_string(),
        body,
        meta,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::test_support::messages_dir;
    use crate::messages::{cursor_for, next, send};
    use tempfile::tempdir;

    #[test]
    fn check_no_inbox_returns_empty() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let summaries = check(&mdir, "login", "reviewer").unwrap();
        assert!(summaries.is_empty());
    }

    #[test]
    fn check_shows_unread_count() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg 1").unwrap();
        send(&mdir, "login", "reviewer", "implementer", "msg 2").unwrap();

        let summaries = check(&mdir, "login", "reviewer").unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].sender, "implementer");
        assert_eq!(summaries[0].count, 2);
    }

    #[test]
    fn check_multiple_senders() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();
        send(&mdir, "login", "reviewer", "user", "msg 1").unwrap();
        send(&mdir, "login", "reviewer", "user", "msg 2").unwrap();

        let summaries = check(&mdir, "login", "reviewer").unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].sender, "implementer");
        assert_eq!(summaries[0].count, 1);
        assert_eq!(summaries[1].sender, "user");
        assert_eq!(summaries[1].count, 2);
    }

    #[test]
    fn read_at_returns_message_by_absolute_index() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "hello").unwrap();
        send(&mdir, "login", "reviewer", "implementer", "world").unwrap();

        let m = read_at(&mdir, "login", "reviewer", "implementer", 1)
            .unwrap()
            .unwrap();
        assert_eq!(m.index, 1);
        assert_eq!(m.body, "hello");
        assert_eq!(m.sender, "implementer");

        let m = read_at(&mdir, "login", "reviewer", "implementer", 2)
            .unwrap()
            .unwrap();
        assert_eq!(m.index, 2);
        assert_eq!(m.body, "world");
    }

    #[test]
    fn read_at_is_pure() {
        // Calling read_at should never touch the cursor.
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();
        assert_eq!(
            cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            0
        );

        read_at(&mdir, "login", "reviewer", "implementer", 1).unwrap();
        read_at(&mdir, "login", "reviewer", "implementer", 1).unwrap();
        read_at(&mdir, "login", "reviewer", "implementer", 1).unwrap();

        assert_eq!(
            cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            0
        );
    }

    #[test]
    fn read_at_nonexistent_index_returns_none() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "only one").unwrap();

        // Index 0 is reserved for "nothing" and should never resolve.
        assert!(
            read_at(&mdir, "login", "reviewer", "implementer", 0)
                .unwrap()
                .is_none()
        );
        // Beyond the last sent message.
        assert!(
            read_at(&mdir, "login", "reviewer", "implementer", 2)
                .unwrap()
                .is_none()
        );
        // Sender that never sent anything.
        assert!(
            read_at(&mdir, "login", "reviewer", "nobody", 1)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn read_at_no_inbox_returns_none() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let m = read_at(&mdir, "login", "reviewer", "implementer", 1).unwrap();
        assert!(m.is_none());
    }

    #[test]
    fn separate_features_are_isolated() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "login msg").unwrap();
        send(&mdir, "signup", "reviewer", "implementer", "signup msg").unwrap();

        let login = read_at(&mdir, "login", "reviewer", "implementer", 1)
            .unwrap()
            .unwrap();
        assert_eq!(login.body, "login msg");

        let signup = read_at(&mdir, "signup", "reviewer", "implementer", 1)
            .unwrap()
            .unwrap();
        assert_eq!(signup.body, "signup msg");

        // Advancing one feature's cursor does not affect the other.
        next(&mdir, "login", "reviewer", "implementer").unwrap();
        assert_eq!(
            cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            1
        );
        assert_eq!(
            cursor_for(&mdir, "signup", "reviewer", "implementer").unwrap(),
            0
        );
    }
}
