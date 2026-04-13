//! `pm hooks stop` — the Stop hook command invoked by Claude Code.
//!
//! Two cases:
//!
//! - **No `.waiting` lock file** → `block` + "run `pm msg wait`". The agent
//!   runs `pm msg wait` which either returns immediately (unread messages
//!   present) or blocks until one arrives. Either way the agent processes
//!   the message, and the cycle repeats.
//! - **`.waiting` lock file exists** → `approve`. A background `pm msg wait`
//!   is already running. Claude stops; the background task will wake it via
//!   a task-notification when a message arrives.
//!
//! The lock file is managed by `pm msg wait` itself (written on start,
//! removed on exit). Registered in `main/.claude/settings.json` by
//! `pm hooks install`.

use serde_json::json;

use crate::commands::agent_wait;
use crate::state::paths;

/// Reason text when no background wait is running.
const REASON: &str = "Run `pm msg wait` to wait for messages..";

/// Run the Stop hook logic. Prints JSON to stdout and returns the exit code.
///
/// Resolution: `PM_AGENT_NAME` for agent identity, CWD → project root →
/// feature name for scope. If any resolution fails (e.g. not in a pm
/// project, no agent name set), approve the stop — the hook should be
/// invisible to non-pm sessions.
pub fn stop() -> i32 {
    match stop_inner() {
        Ok(json) => {
            print!("{json}");
            0
        }
        Err(_) => {
            // Resolution failed — not a pm agent session. Approve stop.
            print!("{}", json!({"decision": "approve"}));
            0
        }
    }
}

fn stop_inner() -> crate::error::Result<String> {
    let agent = std::env::var("PM_AGENT_NAME")
        .map_err(|_| crate::error::PmError::Messaging("no PM_AGENT_NAME".into()))?;

    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let feature = resolve_feature_or_scope(&project_root, &cwd)?;

    let messages_dir = paths::messages_dir(&project_root);

    if agent_wait::is_waiting(&messages_dir, &feature, &agent) {
        Ok(json!({"decision": "approve"}).to_string())
    } else {
        Ok(json!({"decision": "block", "reason": REASON}).to_string())
    }
}

/// Resolve the current scope: feature name from CWD, or "main".
fn resolve_feature_or_scope(
    project_root: &std::path::Path,
    cwd: &std::path::Path,
) -> crate::error::Result<String> {
    if let Some(feature) = paths::detect_feature_from_cwd(project_root, cwd) {
        return Ok(feature);
    }
    if paths::is_in_main_worktree(project_root, cwd) {
        return Ok("main".to_string());
    }
    Err(crate::error::PmError::NotInWorktree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup(dir: &std::path::Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        std::fs::write(root.join(".pm/features/login.toml"), "").unwrap();
        paths::messages_dir(&root)
    }

    /// Test the decision logic directly, bypassing env-var / CWD resolution.
    fn decide(messages_dir: &std::path::Path, feature: &str, agent: &str) -> &'static str {
        if agent_wait::is_waiting(messages_dir, feature, agent) {
            "approve"
        } else {
            "block"
        }
    }

    #[test]
    fn no_lock_blocks() {
        let dir = tempdir().unwrap();
        let mdir = setup(dir.path());
        assert_eq!(decide(&mdir, "login", "reviewer"), "block");
    }

    #[test]
    fn lock_present_approves() {
        let dir = tempdir().unwrap();
        let mdir = setup(dir.path());

        let lock = agent_wait::waiting_lock_path(&mdir, "login", "reviewer");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        // Use our own PID so the liveness check passes.
        std::fs::write(&lock, std::process::id().to_string()).unwrap();

        assert_eq!(decide(&mdir, "login", "reviewer"), "approve");
    }
}
