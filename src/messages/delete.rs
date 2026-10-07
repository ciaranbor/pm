//! Removing a feature's or an agent's messages outright.

use std::path::Path;

use super::layout::inbox_dir;
use crate::error::Result;

/// Delete all messages for a feature. No-op if the directory doesn't exist.
pub fn delete_feature(messages_dir: &Path, feature: &str) -> Result<()> {
    let dir = messages_dir.join(feature);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Delete a single agent's inbox within a feature (queues, `.meta/`,
/// `.cursor`, `.last_read`). No-op if the directory doesn't exist.
/// Used by `pm agent delete` to ensure no stale state is left behind
/// for a registry entry that has been permanently removed.
pub fn delete_inbox(messages_dir: &Path, feature: &str, agent: &str) -> Result<()> {
    let dir = inbox_dir(messages_dir, feature, agent);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::send;
    use crate::messages::test_support::messages_dir;
    use crate::messages::{LastRead, save_last_read};
    use tempfile::tempdir;

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
    fn delete_inbox_removes_agent_dir_only() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg 1").unwrap();
        send(&mdir, "login", "implementer", "reviewer", "msg 2").unwrap();

        let lr = LastRead {
            sender: "implementer".to_string(),
            sender_scope: None,
            sender_project: None,
            index: 1,
        };
        save_last_read(&mdir, "login", "reviewer", &lr).unwrap();

        let reviewer_inbox = inbox_dir(&mdir, "login", "reviewer");
        let implementer_inbox = inbox_dir(&mdir, "login", "implementer");
        assert!(reviewer_inbox.exists());
        assert!(implementer_inbox.exists());

        delete_inbox(&mdir, "login", "reviewer").unwrap();

        // reviewer's inbox is gone (queues, .meta, .last_read all wiped)
        assert!(!reviewer_inbox.exists());
        // other agents in the same feature are untouched
        assert!(implementer_inbox.exists());
        // the feature directory itself is preserved
        assert!(mdir.join("login").exists());
    }

    #[test]
    fn delete_inbox_missing_is_ok() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        // Should not error when the inbox doesn't exist
        delete_inbox(&mdir, "login", "ghost").unwrap();
    }
}
