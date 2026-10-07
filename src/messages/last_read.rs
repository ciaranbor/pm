//! The `.last_read` record of the most recently read message, which
//! `pm msg reply` routes back to.

use std::path::{Path, PathBuf};

use super::layout::inbox_dir;
use super::types::LastRead;
use crate::error::Result;

/// Returns the path to the `.last_read` file in an agent's inbox.
fn last_read_path(messages_dir: &Path, feature: &str, agent: &str) -> PathBuf {
    inbox_dir(messages_dir, feature, agent).join(".last_read")
}

/// Save the last-read metadata for an agent's inbox.
pub fn save_last_read(
    messages_dir: &Path,
    feature: &str,
    agent: &str,
    last_read: &LastRead,
) -> Result<()> {
    let path = last_read_path(messages_dir, feature, agent);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(last_read)?;
    let tmp = path.with_file_name(".last_read.tmp");
    std::fs::write(&tmp, &content)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Load the last-read metadata for an agent's inbox.
/// Returns `None` if no message has been read yet.
pub fn load_last_read(messages_dir: &Path, feature: &str, agent: &str) -> Result<Option<LastRead>> {
    let path = last_read_path(messages_dir, feature, agent);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let last_read: LastRead = serde_json::from_str(&content)?;
    Ok(Some(last_read))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::test_support::messages_dir;
    use tempfile::tempdir;

    #[test]
    fn save_load_last_read_round_trip() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let lr = LastRead {
            sender: "reviewer".to_string(),
            sender_scope: Some("main".to_string()),
            sender_project: None,
            index: 3,
        };
        save_last_read(&mdir, "login", "implementer", &lr).unwrap();

        let loaded = load_last_read(&mdir, "login", "implementer")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.sender, "reviewer");
        assert_eq!(loaded.sender_scope.as_deref(), Some("main"));
        assert_eq!(loaded.sender_project, None);
        assert_eq!(loaded.index, 3);
    }

    #[test]
    fn load_last_read_returns_none_when_no_file() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let result = load_last_read(&mdir, "login", "implementer").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn save_last_read_overwrites_previous() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let lr1 = LastRead {
            sender: "reviewer".to_string(),
            sender_scope: Some("main".to_string()),
            sender_project: None,
            index: 1,
        };
        save_last_read(&mdir, "login", "implementer", &lr1).unwrap();

        let lr2 = LastRead {
            sender: "user".to_string(),
            sender_scope: Some("other-feat".to_string()),
            sender_project: Some("exo".to_string()),
            index: 5,
        };
        save_last_read(&mdir, "login", "implementer", &lr2).unwrap();

        let loaded = load_last_read(&mdir, "login", "implementer")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.sender, "user");
        assert_eq!(loaded.sender_scope.as_deref(), Some("other-feat"));
        assert_eq!(loaded.sender_project.as_deref(), Some("exo"));
        assert_eq!(loaded.index, 5);
    }
}
