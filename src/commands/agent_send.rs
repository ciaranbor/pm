use std::path::Path;

use crate::error::Result;
use crate::messages;
use crate::state::paths;

/// Send a message to an agent's inbox. Returns a status line.
pub fn agent_send(
    project_root: &Path,
    feature: &str,
    recipient: &str,
    sender: &str,
    body: &str,
) -> Result<String> {
    let messages_dir = paths::messages_dir(project_root);
    let index = messages::send(&messages_dir, feature, recipient, sender, body)?;
    Ok(format!(
        "Message {index:03} sent to '{recipient}' (from '{sender}')"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn setup_project(dir: &Path) -> PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        root
    }

    #[test]
    fn send_returns_confirmation() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let msg = agent_send(&root, "login", "reviewer", "implementer", "hello").unwrap();
        assert_eq!(msg, "Message 001 sent to 'reviewer' (from 'implementer')");
    }

    #[test]
    fn send_increments_index_in_output() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        agent_send(&root, "login", "reviewer", "implementer", "first").unwrap();
        let msg = agent_send(&root, "login", "reviewer", "implementer", "second").unwrap();
        assert!(msg.contains("Message 002"));
    }
}
