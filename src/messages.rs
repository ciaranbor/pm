use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PmError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageMeta {
    pub sender: String,
    pub timestamp: DateTime<Utc>,
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

/// Default user name from $USER environment variable.
pub fn default_user_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".to_string())
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
pub fn send(
    messages_dir: &Path,
    feature: &str,
    recipient: &str,
    sender: &str,
    body: &str,
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

/// Read unread messages from an agent's inbox. Advances the cursor.
/// If `from` is specified, only reads from that sender.
pub fn read(
    messages_dir: &Path,
    feature: &str,
    agent: &str,
    from: Option<&str>,
) -> Result<Vec<Message>> {
    let inbox = inbox_dir(messages_dir, feature, agent);
    if !inbox.exists() {
        return Ok(Vec::new());
    }

    let cpath = cursor_path(messages_dir, feature, agent);
    let mut cursor = load_cursor(&cpath)?;
    let mut messages = Vec::new();

    let senders = match from {
        Some(sender) => vec![sender.to_string()],
        None => list_senders(&inbox)?,
    };

    for sender in &senders {
        let sdir = sender_dir(messages_dir, feature, agent, sender);
        if !sdir.exists() {
            continue;
        }
        let last_read = cursor.get(sender.as_str()).copied().unwrap_or(0);
        let latest = max_index(&sdir)?;

        for i in (last_read + 1)..=latest {
            let msg_path = sdir.join(format!("{i:03}.md"));
            let meta_path =
                meta_dir(messages_dir, feature, agent, sender).join(format!("{i:03}.json"));

            if !msg_path.exists() {
                continue;
            }

            let body = std::fs::read_to_string(&msg_path)?;
            let meta = if meta_path.exists() {
                let content = std::fs::read_to_string(&meta_path)?;
                serde_json::from_str(&content)?
            } else {
                MessageMeta {
                    sender: sender.clone(),
                    timestamp: Utc::now(),
                }
            };

            messages.push(Message {
                index: i,
                sender: sender.clone(),
                body,
                meta,
            });
        }

        if latest > last_read {
            cursor.insert(sender.clone(), latest);
        }
    }

    save_cursor(&cpath, &cursor)?;
    Ok(messages)
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

    #[test]
    fn check_after_read_shows_zero() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();
        read(&mdir, "login", "reviewer", None).unwrap();

        let summaries = check(&mdir, "login", "reviewer").unwrap();
        assert!(summaries.is_empty());
    }

    #[test]
    fn read_returns_unread_messages() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "hello").unwrap();
        send(&mdir, "login", "reviewer", "implementer", "world").unwrap();

        let msgs = read(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].body, "hello");
        assert_eq!(msgs[0].index, 1);
        assert_eq!(msgs[0].sender, "implementer");
        assert_eq!(msgs[1].body, "world");
        assert_eq!(msgs[1].index, 2);
    }

    #[test]
    fn read_advances_cursor() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "first").unwrap();
        let msgs = read(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(msgs.len(), 1);

        // Second read returns nothing
        let msgs = read(&mdir, "login", "reviewer", None).unwrap();
        assert!(msgs.is_empty());

        // New message appears
        send(&mdir, "login", "reviewer", "implementer", "second").unwrap();
        let msgs = read(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].body, "second");
        assert_eq!(msgs[0].index, 2);
    }

    #[test]
    fn read_with_sender_filter() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "from impl").unwrap();
        send(&mdir, "login", "reviewer", "user", "from user").unwrap();

        let msgs = read(&mdir, "login", "reviewer", Some("implementer")).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "implementer");
        assert_eq!(msgs[0].body, "from impl");

        // "user" messages are still unread
        let summaries = check(&mdir, "login", "reviewer").unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].sender, "user");
    }

    #[test]
    fn read_no_inbox_returns_empty() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let msgs = read(&mdir, "login", "reviewer", None).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn read_from_nonexistent_sender_returns_empty() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();
        let msgs = read(&mdir, "login", "reviewer", Some("nobody")).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn separate_features_are_isolated() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "login msg").unwrap();
        send(&mdir, "signup", "reviewer", "implementer", "signup msg").unwrap();

        let login_msgs = read(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(login_msgs.len(), 1);
        assert_eq!(login_msgs[0].body, "login msg");

        let signup_msgs = read(&mdir, "signup", "reviewer", None).unwrap();
        assert_eq!(signup_msgs.len(), 1);
        assert_eq!(signup_msgs[0].body, "signup msg");
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
