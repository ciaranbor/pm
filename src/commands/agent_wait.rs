use std::path::Path;
use std::time::Duration;

use crate::error::Result;
use crate::messages;
use crate::state::paths;

/// Poll for new messages in an agent's inbox, blocking until at least one arrives.
/// Returns the unread count when messages are found.
pub fn agent_wait(
    project_root: &Path,
    feature: &str,
    agent: &str,
    poll_interval: Option<Duration>,
) -> Result<u32> {
    let messages_dir = paths::messages_dir(project_root);
    let interval = poll_interval.unwrap_or(Duration::from_secs(2));

    loop {
        let summaries = messages::check(&messages_dir, feature, agent)?;
        let total: u32 = summaries.iter().map(|s| s.count).sum();

        if total > 0 {
            return Ok(total);
        }

        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn setup_project(dir: &Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        std::fs::write(root.join(".pm/features/login.toml"), "").unwrap();
        root
    }

    #[test]
    fn wait_returns_immediately_when_messages_exist() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "hi").unwrap();

        let count =
            agent_wait(&root, "login", "reviewer", Some(Duration::from_millis(50))).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn wait_returns_total_across_senders() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "two").unwrap();
        messages::send(&mdir, "login", "reviewer", "user", "three").unwrap();

        let count =
            agent_wait(&root, "login", "reviewer", Some(Duration::from_millis(50))).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn wait_blocks_until_message_arrives() {
        let dir = tempdir().unwrap();
        let root = Arc::new(setup_project(dir.path()));

        let root_clone = Arc::clone(&root);
        let handle = std::thread::spawn(move || {
            agent_wait(
                &root_clone,
                "login",
                "reviewer",
                Some(Duration::from_millis(50)),
            )
            .unwrap()
        });

        // Small delay then send a message
        std::thread::sleep(Duration::from_millis(150));
        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "hello").unwrap();

        let count = handle.join().unwrap();
        assert_eq!(count, 1);
    }
}
