use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::Result;
use crate::messages;
use crate::state::paths;

/// Path to the waiting lock file for a given agent inbox. The Stop hook
/// checks for this file to know whether a background `pm msg wait` is
/// already running.
pub fn waiting_lock_path(messages_dir: &Path, feature: &str, agent: &str) -> PathBuf {
    messages_dir.join(feature).join(agent).join(".waiting")
}

/// Check whether a live `pm msg wait` is running for this agent. Reads the
/// PID from the lock file and checks if that process is still alive. Stale
/// lock files (process died without cleanup, e.g. `tmux kill-window`) are
/// automatically removed.
pub fn is_waiting(messages_dir: &Path, feature: &str, agent: &str) -> bool {
    let lock = waiting_lock_path(messages_dir, feature, agent);
    let Ok(content) = std::fs::read_to_string(&lock) else {
        return false;
    };
    let Ok(pid) = content.trim().parse::<i32>() else {
        // Corrupt lock file — remove it.
        let _ = std::fs::remove_file(&lock);
        return false;
    };
    // SAFETY: `kill(pid, 0)` sends no signal — it only checks whether `pid`
    // exists and is reachable. No side effects beyond updating errno.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    if !alive {
        let _ = std::fs::remove_file(&lock);
    }
    alive
}

/// Poll for new messages in an agent's inbox, blocking until at least one
/// arrives. Returns the total unread count when messages are found. If
/// `from` is specified, only messages from that sender count.
///
/// While polling, a `.waiting` lock file is held in the agent's inbox
/// directory. The Stop hook (`pm hooks stop`) checks for this file to
/// decide whether to allow Claude to stop (background wait running) or
/// block (no wait running, agent must start one).
pub fn agent_wait(
    project_root: &Path,
    feature: &str,
    agent: &str,
    from: Option<&str>,
    poll_interval: Option<Duration>,
) -> Result<u32> {
    let messages_dir = paths::messages_dir(project_root);
    let interval = poll_interval.unwrap_or(Duration::from_secs(2));

    // Write lock file so the Stop hook knows a wait is active.
    let lock = waiting_lock_path(&messages_dir, feature, agent);
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&lock, std::process::id().to_string())?;

    let result = poll_loop(&messages_dir, feature, agent, from, interval);

    // Always remove the lock, even on error.
    let _ = std::fs::remove_file(&lock);

    result
}

fn poll_loop(
    messages_dir: &Path,
    feature: &str,
    agent: &str,
    from: Option<&str>,
    interval: Duration,
) -> Result<u32> {
    loop {
        let summaries = messages::check(messages_dir, feature, agent)?;
        let total: u32 = summaries
            .iter()
            .filter(|s| from.is_none_or(|f| s.sender == f))
            .map(|s| s.count)
            .sum();

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

        let count = agent_wait(
            &root,
            "login",
            "reviewer",
            None,
            Some(Duration::from_millis(50)),
        )
        .unwrap();
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

        let count = agent_wait(
            &root,
            "login",
            "reviewer",
            None,
            Some(Duration::from_millis(50)),
        )
        .unwrap();
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
                None,
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

    #[test]
    fn wait_with_from_ignores_other_senders() {
        let dir = tempdir().unwrap();
        let root = Arc::new(setup_project(dir.path()));

        let mdir = paths::messages_dir(&root);
        // Noise from someone we don't care about.
        messages::send(&mdir, "login", "reviewer", "user", "noise").unwrap();

        let root_clone = Arc::clone(&root);
        let handle = std::thread::spawn(move || {
            agent_wait(
                &root_clone,
                "login",
                "reviewer",
                Some("implementer"),
                Some(Duration::from_millis(50)),
            )
            .unwrap()
        });

        // The "noise" from user should not unblock the wait.
        std::thread::sleep(Duration::from_millis(150));
        messages::send(&mdir, "login", "reviewer", "implementer", "real").unwrap();

        let count = handle.join().unwrap();
        assert_eq!(count, 1);
    }
}
