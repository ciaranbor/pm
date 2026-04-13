//! `pm hooks stop` — the Stop hook command invoked by Claude Code.
//!
//! The hook blocks until the agent has unread messages, then returns
//! `{"decision":"block","reason":"You have new messages. Run `pm msg read` …"}`
//! which Claude Code delivers as a continuation prompt. The agent reads
//! the message, processes it, the turn ends, and the hook fires again.
//!
//! This is simpler than the old two-path lock-file design: the hook
//! itself calls `agent_wait`, so Claude just sees "you have messages"
//! as a user message after every idle period.

use std::time::Duration;

use serde_json::json;

use crate::commands::agent_wait;
use crate::state::paths;

/// Reason text returned after messages arrive.
const REASON: &str = "You have new messages. Run `pm msg read` to read them.";

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

    wait_and_decide(&project_root, &feature, &agent, None)
}

/// Block until messages are available, then return the JSON decision.
/// Extracted from `stop_inner` so tests can call it with explicit paths
/// and a short poll interval.
fn wait_and_decide(
    project_root: &std::path::Path,
    feature: &str,
    agent: &str,
    poll_interval: Option<Duration>,
) -> crate::error::Result<String> {
    agent_wait::agent_wait(project_root, feature, agent, None, poll_interval)?;
    Ok(json!({"decision": "block", "reason": REASON}).to_string())
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
    use std::sync::Arc;
    use tempfile::tempdir;

    fn setup_project(dir: &std::path::Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        std::fs::write(root.join(".pm/features/login.toml"), "").unwrap();
        root
    }

    #[test]
    fn returns_block_with_reason_when_messages_exist() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        crate::messages::send(&mdir, "login", "reviewer", "implementer", "hi").unwrap();

        let result =
            wait_and_decide(&root, "login", "reviewer", Some(Duration::from_millis(50))).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "block");
        assert_eq!(parsed["reason"], REASON);
    }

    #[test]
    fn blocks_then_returns_block_when_message_arrives() {
        let dir = tempdir().unwrap();
        let root = Arc::new(setup_project(dir.path()));

        let root_clone = Arc::clone(&root);
        let handle = std::thread::spawn(move || {
            wait_and_decide(
                &root_clone,
                "login",
                "reviewer",
                Some(Duration::from_millis(50)),
            )
            .unwrap()
        });

        // Small delay then send a message.
        std::thread::sleep(Duration::from_millis(150));
        let mdir = paths::messages_dir(&root);
        crate::messages::send(&mdir, "login", "reviewer", "implementer", "hello").unwrap();

        let result = handle.join().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "block");
        assert_eq!(parsed["reason"], REASON);
    }

    #[test]
    fn stop_inner_fails_without_agent_env() {
        // Ensure PM_AGENT_NAME is not set — stop_inner should error.
        // SAFETY: Only stop_inner reads PM_AGENT_NAME in this binary. Fragile
        // if another test starts reading it concurrently — revisit if that happens.
        unsafe { std::env::remove_var("PM_AGENT_NAME") };
        assert!(stop_inner().is_err());
    }
}
