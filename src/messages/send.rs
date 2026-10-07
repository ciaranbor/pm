//! Delivering a message into a recipient's queue for its sender.

use std::path::Path;

use chrono::Utc;

use super::layout::{max_index, meta_dir, sender_dir};
use super::types::MessageMeta;
use super::validation::validate_name;
use crate::error::Result;

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

/// Like [`send`], but records the sender's scope and optionally project in metadata.
pub fn send_with_scope(
    messages_dir: &Path,
    feature: &str,
    recipient: &str,
    sender: &str,
    body: &str,
    sender_scope: Option<&str>,
) -> Result<u32> {
    send_full(
        messages_dir,
        feature,
        recipient,
        sender,
        body,
        sender_scope,
        None,
    )
}

/// Full send with all metadata fields. Use [`send`] or [`send_with_scope`] for common cases.
pub fn send_full(
    messages_dir: &Path,
    feature: &str,
    recipient: &str,
    sender: &str,
    body: &str,
    sender_scope: Option<&str>,
    sender_project: Option<&str>,
) -> Result<u32> {
    validate_name(feature, "feature")?;
    validate_name(recipient, "recipient")?;
    validate_name(sender, "sender")?;

    let sdir = sender_dir(messages_dir, feature, recipient, sender);
    let mdir = meta_dir(messages_dir, feature, recipient, sender);
    std::fs::create_dir_all(&sdir)?;
    std::fs::create_dir_all(&mdir)?;

    let meta = MessageMeta {
        sender: sender.to_string(),
        timestamp: Utc::now(),
        sender_scope: sender_scope.map(|s| s.to_string()),
        sender_project: sender_project.map(|s| s.to_string()),
    };

    // Sends from one sender take turns, so an index becomes visible only
    // after every lower one. Readers find a message by its body file, so the
    // meta goes in first and each file is renamed in whole. A send that dies
    // part way leaves at most a meta with no body, which the next send
    // overwrites; the lock goes with the process.
    let lock = std::fs::File::create(mdir.join(".lock"))?;
    lock.lock()?;
    let index = max_index(&sdir)? + 1;
    write_whole(
        &mdir,
        &format!("{index:03}.json"),
        serde_json::to_string_pretty(&meta)?.as_bytes(),
    )?;
    write_whole(&sdir, &format!("{index:03}.md"), body.as_bytes())?;

    Ok(index)
}

/// Replace `dir/name` in one rename, so a reader sees the old or the new
/// content, never a partial write.
fn write_whole(dir: &Path, name: &str, content: &[u8]) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut file, content)?;
    file.persist(dir.join(name)).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PmError;
    use crate::messages::test_support::messages_dir;
    use crate::messages::{cursor_for, list, next, read_at};
    use tempfile::tempdir;

    #[test]
    fn a_send_after_one_that_died_part_way_is_delivered() {
        let dir = tempdir().unwrap();
        let messages_dir = dir.path();
        send(messages_dir, "login", "reviewer", "user", "first").unwrap();
        next(messages_dir, "login", "reviewer", "user").unwrap();
        std::fs::write(
            meta_dir(messages_dir, "login", "reviewer", "user").join("002.json"),
            "{",
        )
        .unwrap();

        send(messages_dir, "login", "reviewer", "user", "second").unwrap();

        let cursor = cursor_for(messages_dir, "login", "reviewer", "user").unwrap();
        let read = read_at(messages_dir, "login", "reviewer", "user", cursor + 1)
            .unwrap()
            .unwrap();
        assert_eq!(read.body, "second");
    }

    #[test]
    fn concurrent_sends_and_reads_never_fail_or_lose_a_message() {
        let dir = tempdir().unwrap();
        let messages_dir = dir.path().to_path_buf();
        send(&messages_dir, "login", "reviewer", "user", "first").unwrap();
        let per_sender = 100;
        let senders: Vec<_> = (0..2)
            .map(|_| {
                let messages_dir = messages_dir.clone();
                std::thread::spawn(move || {
                    for i in 0..per_sender {
                        send(&messages_dir, "login", "reviewer", "user", &format!("m{i}")).unwrap();
                    }
                })
            })
            .collect();
        while !senders.iter().all(|t| t.is_finished()) {
            let listed = list(&messages_dir, "login", "reviewer", None).unwrap();
            let latest = listed.last().unwrap().index;
            read_at(&messages_dir, "login", "reviewer", "user", latest).unwrap();
        }
        for t in senders {
            t.join().unwrap();
        }

        let listed = list(&messages_dir, "login", "reviewer", None).unwrap();
        assert_eq!(listed.len(), 1 + 2 * per_sender);
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
}
