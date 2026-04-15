use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PmError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageMeta {
    pub sender: String,
    pub timestamp: DateTime<Utc>,
    /// The scope (feature name or "main") the sender was in when the message
    /// was sent. `None` for messages sent before cross-scope support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_scope: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub index: u32,
    pub sender: String,
    pub body: String,
    pub meta: MessageMeta,
}

#[derive(Debug, Clone)]
pub struct UnreadSummary {
    pub sender: String,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    /// Already processed (index <= cursor).
    Read,
    /// The next message to be read (index == cursor + 1).
    Next,
    /// Ahead of the cursor (index > cursor + 1).
    Queued,
}

#[derive(Debug, Clone)]
pub struct MessageSummary {
    pub sender: String,
    pub index: u32,
    pub timestamp: DateTime<Utc>,
    pub first_line: String,
    pub status: MessageStatus,
}

/// Result of resolving which sender a command operates on when `--from`
/// is not specified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenderResolution {
    /// `--from` was given explicitly.
    Explicit(String),
    /// Exactly one sender has unread messages; use it.
    Implicit(String),
    /// The inbox has no unread messages at all.
    NoUnread,
    /// More than one sender has unread messages; caller must disambiguate.
    Ambiguous(Vec<String>),
}

impl SenderResolution {
    /// Return the resolved sender, or a messaging error describing the
    /// reason resolution failed.
    pub fn into_sender(self) -> Result<String> {
        match self {
            SenderResolution::Explicit(s) | SenderResolution::Implicit(s) => Ok(s),
            SenderResolution::NoUnread => Err(PmError::Messaging("No new messages".to_string())),
            SenderResolution::Ambiguous(senders) => Err(PmError::Messaging(format!(
                "messages from multiple senders are unread, specify --from {{{}}}",
                senders.join(",")
            ))),
        }
    }
}

/// Default identity: PM_AGENT_NAME (set by `pm agent spawn`) > $USER > "user".
pub fn default_user_name() -> String {
    std::env::var("PM_AGENT_NAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".to_string())
}

/// Cursor tracks the last-read index per sender.
type Cursor = BTreeMap<String, u32>;

/// Validate that a name (agent, sender, feature) is safe for use as a path component.
/// Allows alphanumeric, dashes, and underscores only.
pub fn validate_name(name: &str, kind: &str) -> Result<()> {
    if name.is_empty() {
        return Err(PmError::InvalidAgentName(format!(
            "{kind} name cannot be empty"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(PmError::InvalidAgentName(format!(
            "{kind} name '{name}' contains invalid characters (only alphanumeric, dashes, and underscores allowed)"
        )));
    }
    Ok(())
}

/// Returns the inbox directory for an agent in a feature.
fn inbox_dir(messages_dir: &Path, feature: &str, agent: &str) -> PathBuf {
    messages_dir.join(feature).join(agent)
}

/// Returns the sender subdirectory within an agent's inbox.
fn sender_dir(messages_dir: &Path, feature: &str, agent: &str, sender: &str) -> PathBuf {
    inbox_dir(messages_dir, feature, agent).join(format!("from-{sender}"))
}

/// Returns the meta directory for a sender within an agent's inbox.
fn meta_dir(messages_dir: &Path, feature: &str, agent: &str, sender: &str) -> PathBuf {
    inbox_dir(messages_dir, feature, agent)
        .join(".meta")
        .join(format!("from-{sender}"))
}

/// Returns the cursor file path for an agent's inbox.
fn cursor_path(messages_dir: &Path, feature: &str, agent: &str) -> PathBuf {
    inbox_dir(messages_dir, feature, agent).join(".cursor")
}

/// Find the highest message index in a directory of numbered .md files.
/// Returns 0 if the directory is empty or doesn't exist.
fn max_index(dir: &Path) -> Result<u32> {
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

fn load_cursor(path: &Path) -> Result<Cursor> {
    if !path.exists() {
        return Ok(Cursor::new());
    }
    let content = std::fs::read_to_string(path)?;
    let cursor: Cursor = serde_json::from_str(&content)?;
    Ok(cursor)
}

fn save_cursor(path: &Path, cursor: &Cursor) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(cursor)?;
    let tmp = path.with_extension("cursor.tmp");
    std::fs::write(&tmp, &content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Send a message to an agent's inbox. Returns the message index.
///
/// `sender_scope` is the scope (feature name or "main") the sender is in.
/// Stored in message metadata so the recipient knows where the message
/// originated. Pass `None` for same-scope messages (backward compat).
pub fn send(
    messages_dir: &Path,
    feature: &str,
    recipient: &str,
    sender: &str,
    body: &str,
) -> Result<u32> {
    send_with_scope(messages_dir, feature, recipient, sender, body, None)
}

/// Like [`send`], but records the sender's scope in metadata.
pub fn send_with_scope(
    messages_dir: &Path,
    feature: &str,
    recipient: &str,
    sender: &str,
    body: &str,
    sender_scope: Option<&str>,
) -> Result<u32> {
    validate_name(feature, "feature")?;
    validate_name(recipient, "recipient")?;
    validate_name(sender, "sender")?;

    let sdir = sender_dir(messages_dir, feature, recipient, sender);
    let mdir = meta_dir(messages_dir, feature, recipient, sender);
    std::fs::create_dir_all(&sdir)?;
    std::fs::create_dir_all(&mdir)?;

    let index = max_index(&sdir)? + 1;
    let msg_path = sdir.join(format!("{index:03}.md"));
    let meta_path = mdir.join(format!("{index:03}.json"));

    let meta = MessageMeta {
        sender: sender.to_string(),
        timestamp: Utc::now(),
        sender_scope: sender_scope.map(|s| s.to_string()),
    };

    std::fs::write(&msg_path, body)?;
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;

    Ok(index)
}

/// Check for unread messages in an agent's inbox. Returns unread counts per sender.
pub fn check(messages_dir: &Path, feature: &str, agent: &str) -> Result<Vec<UnreadSummary>> {
    let inbox = inbox_dir(messages_dir, feature, agent);
    if !inbox.exists() {
        return Ok(Vec::new());
    }

    let cursor = load_cursor(&cursor_path(messages_dir, feature, agent))?;
    let mut summaries = Vec::new();

    for entry in std::fs::read_dir(&inbox)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if let Some(sender) = name.strip_prefix("from-") {
            if !entry.path().is_dir() {
                continue;
            }
            let last_read = cursor.get(sender).copied().unwrap_or(0);
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

/// Read the cursor position (last-processed index) for one sender in an
/// agent's inbox. Returns 0 if the sender has never sent a message or the
/// cursor has not been advanced.
pub fn cursor_for(messages_dir: &Path, feature: &str, agent: &str, sender: &str) -> Result<u32> {
    let cursor = load_cursor(&cursor_path(messages_dir, feature, agent))?;
    Ok(cursor.get(sender).copied().unwrap_or(0))
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
    let meta_path = meta_dir(messages_dir, feature, agent, sender).join(format!("{index:03}.json"));
    let meta = if meta_path.exists() {
        let content = std::fs::read_to_string(&meta_path)?;
        serde_json::from_str(&content)?
    } else {
        MessageMeta {
            sender: sender.to_string(),
            timestamp: Utc::now(),
            sender_scope: None,
        }
    };

    Ok(Some(Message {
        index,
        sender: sender.to_string(),
        body,
        meta,
    }))
}

/// Advance the cursor for one sender by exactly one position. Returns the
/// new cursor position. Errors if the cursor is already at the latest
/// index (nothing to advance past).
pub fn next(messages_dir: &Path, feature: &str, agent: &str, sender: &str) -> Result<u32> {
    validate_name(feature, "feature")?;
    validate_name(agent, "agent")?;
    validate_name(sender, "sender")?;

    let sdir = sender_dir(messages_dir, feature, agent, sender);
    let latest = max_index(&sdir)?;

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

/// Resolve which sender a single-message command operates on. If `from` is
/// provided, it is returned verbatim as `Explicit`. Otherwise, the senders
/// with unread messages are examined and the caller gets `Implicit(s)`
/// (exactly one), `NoUnread` (zero), or `Ambiguous(_)` (more than one).
pub fn resolve_sender(
    messages_dir: &Path,
    feature: &str,
    agent: &str,
    from: Option<&str>,
) -> Result<SenderResolution> {
    if let Some(s) = from {
        validate_name(s, "sender")?;
        return Ok(SenderResolution::Explicit(s.to_string()));
    }

    let summaries = check(messages_dir, feature, agent)?;
    match summaries.len() {
        0 => Ok(SenderResolution::NoUnread),
        1 => Ok(SenderResolution::Implicit(summaries[0].sender.clone())),
        _ => Ok(SenderResolution::Ambiguous(
            summaries.into_iter().map(|s| s.sender).collect(),
        )),
    }
}

/// Enumerate all messages in an agent's inbox (optionally scoped to one
/// sender) along with their status relative to the cursor. Returns a list
/// grouped by sender, then by index ascending. Does not touch the cursor.
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

    let cursor = load_cursor(&cursor_path(messages_dir, feature, agent))?;
    let senders = match from {
        Some(s) => {
            validate_name(s, "sender")?;
            vec![s.to_string()]
        }
        None => list_senders(&inbox)?,
    };

    let mut out = Vec::new();
    for sender in &senders {
        let sdir = sender_dir(messages_dir, feature, agent, sender);
        if !sdir.exists() {
            continue;
        }
        let latest = max_index(&sdir)?;
        let cur = cursor.get(sender.as_str()).copied().unwrap_or(0);

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

            let meta_path =
                meta_dir(messages_dir, feature, agent, sender).join(format!("{i:03}.json"));
            let timestamp = if meta_path.exists() {
                let content = std::fs::read_to_string(&meta_path)?;
                let m: MessageMeta = serde_json::from_str(&content)?;
                m.timestamp
            } else {
                Utc::now()
            };

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

/// Delete all messages for a feature. No-op if the directory doesn't exist.
pub fn delete_feature(messages_dir: &Path, feature: &str) -> Result<()> {
    let dir = messages_dir.join(feature);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// List all senders who have sent messages to an inbox.
fn list_senders(inbox: &Path) -> Result<Vec<String>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn messages_dir(dir: &Path) -> PathBuf {
        dir.join("messages")
    }

    #[test]
    fn send_creates_message_file() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let index = send(&mdir, "login", "reviewer", "implementer", "Please review").unwrap();
        assert_eq!(index, 1);

        let msg_path = mdir.join("login/reviewer/from-implementer/001.md");
        assert!(msg_path.exists());
        assert_eq!(std::fs::read_to_string(&msg_path).unwrap(), "Please review");
    }

    #[test]
    fn send_creates_metadata() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "hello").unwrap();

        let meta_path = mdir.join("login/reviewer/.meta/from-implementer/001.json");
        assert!(meta_path.exists());

        let content = std::fs::read_to_string(&meta_path).unwrap();
        let meta: MessageMeta = serde_json::from_str(&content).unwrap();
        assert_eq!(meta.sender, "implementer");
    }

    #[test]
    fn send_increments_index() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let i1 = send(&mdir, "login", "reviewer", "implementer", "first").unwrap();
        let i2 = send(&mdir, "login", "reviewer", "implementer", "second").unwrap();
        let i3 = send(&mdir, "login", "reviewer", "implementer", "third").unwrap();

        assert_eq!(i1, 1);
        assert_eq!(i2, 2);
        assert_eq!(i3, 3);
    }

    #[test]
    fn send_separate_senders_have_independent_indices() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let i1 = send(&mdir, "login", "reviewer", "implementer", "from impl").unwrap();
        let i2 = send(&mdir, "login", "reviewer", "user", "from user").unwrap();

        assert_eq!(i1, 1);
        assert_eq!(i2, 1);
    }

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

    // ----- read_at (pure random-access reads) -----

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

    // ----- next (advance cursor by 1) -----

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

    // ----- resolve_sender (--from resolution) -----

    #[test]
    fn resolve_sender_explicit_passthrough() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let r = resolve_sender(&mdir, "login", "reviewer", Some("implementer")).unwrap();
        assert_eq!(r, SenderResolution::Explicit("implementer".to_string()));
    }

    #[test]
    fn resolve_sender_implicit_when_one_unread() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();
        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, SenderResolution::Implicit("implementer".to_string()));
    }

    #[test]
    fn resolve_sender_no_unread_when_empty() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, SenderResolution::NoUnread);
    }

    #[test]
    fn resolve_sender_no_unread_after_next() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();
        next(&mdir, "login", "reviewer", "implementer").unwrap();

        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, SenderResolution::NoUnread);
    }

    #[test]
    fn resolve_sender_ambiguous_when_multiple_unread() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        send(&mdir, "login", "reviewer", "user", "b").unwrap();

        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        match r {
            SenderResolution::Ambiguous(senders) => {
                assert_eq!(senders, vec!["implementer".to_string(), "user".to_string()]);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn resolve_sender_implicit_picks_the_unread_one() {
        // Two senders exist, but only one has unread. Implicit resolution
        // picks that sender, not "ambiguous".
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        send(&mdir, "login", "reviewer", "user", "b").unwrap();
        next(&mdir, "login", "reviewer", "implementer").unwrap();

        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, SenderResolution::Implicit("user".to_string()));
    }

    #[test]
    fn resolution_into_sender_surfaces_friendly_errors() {
        // NoUnread turns into a "No new messages" error; Ambiguous lists the senders.
        let no_unread = SenderResolution::NoUnread;
        let err = no_unread.into_sender().unwrap_err();
        assert_eq!(format!("{err}"), "No new messages");

        let ambig = SenderResolution::Ambiguous(vec!["a".to_string(), "b".to_string()]);
        let err = ambig.into_sender().unwrap_err();
        assert!(format!("{err}").contains("specify --from"));
        assert!(format!("{err}").contains("a,b"));
    }

    // ----- list -----

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
        next(&mdir, "login", "reviewer", "implementer").unwrap();
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
        assert_eq!(v[0].sender, "implementer");
        assert_eq!(v[0].index, 1);
        assert_eq!(v[1].sender, "implementer");
        assert_eq!(v[1].index, 2);
        assert_eq!(v[2].sender, "user");
        assert_eq!(v[2].index, 1);
    }

    #[test]
    fn list_scoped_to_one_sender() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "impl").unwrap();
        send(&mdir, "login", "reviewer", "user", "user").unwrap();

        let v = list(&mdir, "login", "reviewer", Some("user")).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].sender, "user");
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

    #[test]
    fn validate_rejects_path_traversal() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let result = send(&mdir, "login", "../../../etc", "implementer", "bad");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::InvalidAgentName(_)));
    }

    #[test]
    fn validate_rejects_slashes() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let result = send(&mdir, "login", "reviewer", "foo/bar", "bad");
        assert!(result.is_err());
    }

    #[test]
    fn validate_rejects_dots() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let result = send(&mdir, "login", "reviewer", "foo.bar", "bad");
        assert!(result.is_err());
    }

    #[test]
    fn validate_rejects_empty_name() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let result = send(&mdir, "login", "", "implementer", "bad");
        assert!(result.is_err());
    }

    #[test]
    fn validate_allows_dashes_and_underscores() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let result = send(&mdir, "my-feature", "code_reviewer", "impl-agent", "ok");
        assert!(result.is_ok());
    }

    #[test]
    fn delete_feature_removes_all_messages() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg 1").unwrap();
        send(&mdir, "login", "reviewer", "user", "msg 2").unwrap();
        send(&mdir, "login", "implementer", "reviewer", "msg 3").unwrap();

        assert!(mdir.join("login").exists());
        delete_feature(&mdir, "login").unwrap();
        assert!(!mdir.join("login").exists());
    }

    #[test]
    fn delete_feature_missing_is_ok() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        // Should not error when directory doesn't exist
        delete_feature(&mdir, "nonexistent").unwrap();
    }

    #[test]
    fn delete_feature_does_not_affect_other_features() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "login msg").unwrap();
        send(&mdir, "signup", "reviewer", "implementer", "signup msg").unwrap();

        delete_feature(&mdir, "login").unwrap();

        assert!(!mdir.join("login").exists());
        assert!(mdir.join("signup").exists());
    }

    #[test]
    fn deleted_message_does_not_break_indexing() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg 1").unwrap();
        send(&mdir, "login", "reviewer", "implementer", "msg 2").unwrap();
        let i3 = send(&mdir, "login", "reviewer", "implementer", "msg 3").unwrap();
        assert_eq!(i3, 3);

        // Delete message 002
        let msg2_path = mdir.join("login/reviewer/from-implementer/002.md");
        std::fs::remove_file(&msg2_path).unwrap();

        // Next index should still be 4 (based on max existing)
        let i4 = send(&mdir, "login", "reviewer", "implementer", "msg 4").unwrap();
        assert_eq!(i4, 4);
    }
}
