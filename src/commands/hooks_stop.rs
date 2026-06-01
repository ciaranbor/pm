//! `pm claude hooks stop` — the Stop hook that keeps pm agents never-idle.
//!
//! Decision: queued messages → `block`; else a running background task or
//! active cron → `approve` (don't block, so the running work can finish); else
//! block on `agent_wait` until a message arrives.

use std::io::Read;
use std::time::Duration;

use serde_json::json;

use crate::commands::agent_wait;
use crate::messages;
use crate::state::paths;

/// Reason text returned after messages arrive.
const REASON: &str = "You have new messages. Run `pm msg read` to read them.";

/// Run the Stop hook. Prints the decision JSON and returns the exit code.
/// Non-pm sessions (unresolvable agent/scope) approve, staying invisible.
pub fn stop() -> i32 {
    match stop_inner() {
        Ok(json) => {
            print!("{json}");
            0
        }
        Err(_) => {
            print!("{}", approve_decision());
            0
        }
    }
}

fn stop_inner() -> crate::error::Result<String> {
    // Resolve identity before reading stdin: non-pm sessions bail here, and
    // tests calling `stop_inner` without piped stdin must not block.
    let agent = std::env::var("PM_AGENT_NAME")
        .map_err(|_| crate::error::PmError::Messaging("no PM_AGENT_NAME".into()))?;

    let busy = read_busy_from_stdin();

    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let feature = paths::resolve_scope_from(&project_root, &cwd)?;

    wait_and_decide(busy, &project_root, &feature, &agent, None)
}

/// Decide the Stop outcome. Testable seam: takes an explicit `busy` flag
/// instead of reading stdin. Messages take priority over `busy`.
fn wait_and_decide(
    busy: bool,
    project_root: &std::path::Path,
    feature: &str,
    agent: &str,
    poll_interval: Option<Duration>,
) -> crate::error::Result<String> {
    if count_unread(project_root, feature, agent)? > 0 {
        return Ok(block_decision());
    }
    if busy {
        return Ok(approve_decision());
    }
    agent_wait::agent_wait(project_root, feature, agent, None, poll_interval)?;
    Ok(block_decision())
}

/// Count unread messages across all senders without blocking.
fn count_unread(
    project_root: &std::path::Path,
    feature: &str,
    agent: &str,
) -> crate::error::Result<u32> {
    let messages_dir = paths::messages_dir(project_root);
    let summaries = messages::check(&messages_dir, feature, agent)?;
    Ok(summaries.iter().map(|s| s.count).sum())
}

fn block_decision() -> String {
    json!({"decision": "block", "reason": REASON}).to_string()
}

fn approve_decision() -> String {
    json!({"decision": "approve"}).to_string()
}

/// Read stdin and derive `busy`. Any read/parse failure → not busy, so the
/// hook never crashes or hangs on unexpected input.
fn read_busy_from_stdin() -> bool {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return false;
    }
    parse_busy(&input)
}

/// `busy` if a `background_tasks` entry is running or a `session_crons` entry
/// is active. Gating tasks on "running" (not mere presence) keeps the agent
/// never-idle: a completed task in the payload must not block messages forever.
fn parse_busy(json_str: &str) -> bool {
    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let background_busy = parsed
        .get("background_tasks")
        .and_then(|v| v.as_array())
        .is_some_and(|tasks| tasks.iter().any(is_task_running));

    let cron_busy = parsed
        .get("session_crons")
        .and_then(|v| v.as_array())
        .is_some_and(|crons| crons.iter().any(is_cron_active));

    background_busy || cron_busy
}

fn is_task_running(task: &serde_json::Value) -> bool {
    task.get("status").and_then(|s| s.as_str()) == Some("running")
}

/// Active unless an explicit terminal status; no status counts as active.
fn is_cron_active(cron: &serde_json::Value) -> bool {
    match cron.get("status").and_then(|s| s.as_str()) {
        Some(status) => !is_terminal_status(status),
        None => true,
    }
}

/// Whether a status string denotes a finished/terminal cron.
fn is_terminal_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "canceled" | "expired" | "killed"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;
    use tempfile::tempdir;

    fn setup_project(dir: &std::path::Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        std::fs::write(root.join(".pm/features/login.toml"), "").unwrap();
        root
    }

    fn send(root: &std::path::Path) {
        let mdir = paths::messages_dir(root);
        crate::messages::send(&mdir, "login", "reviewer", "implementer", "hi").unwrap();
    }

    // --- decision matrix -------------------------------------------------

    #[test]
    fn returns_block_with_reason_when_messages_exist() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        send(&root);

        let result = wait_and_decide(
            false,
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(50)),
        )
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "block");
        assert_eq!(parsed["reason"], REASON);
    }

    #[test]
    fn messages_take_priority_over_busy() {
        // Messages queued AND busy → block (messages win).
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        send(&root);

        let result = wait_and_decide(
            true,
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(50)),
        )
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "block");
        assert_eq!(parsed["reason"], REASON);
    }

    #[test]
    fn busy_with_no_messages_approves_promptly() {
        // No messages + busy must approve without entering the unbounded block.
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let start = Instant::now();
        let result = wait_and_decide(
            true,
            &root,
            "login",
            "reviewer",
            // Long interval surfaces any accidental blocking.
            Some(Duration::from_secs(30)),
        )
        .unwrap();
        let elapsed = start.elapsed();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "approve");
        assert!(parsed.get("reason").is_none());
        assert!(
            elapsed < Duration::from_secs(1),
            "busy path must return promptly, took {elapsed:?}"
        );
    }

    #[test]
    fn idle_blocks_until_message_arrives() {
        // No messages, not busy → unbounded block until a message lands.
        let dir = tempdir().unwrap();
        let root = Arc::new(setup_project(dir.path()));

        let root_clone = Arc::clone(&root);
        let handle = std::thread::spawn(move || {
            wait_and_decide(
                false,
                &root_clone,
                "login",
                "reviewer",
                Some(Duration::from_millis(50)),
            )
            .unwrap()
        });

        // Small delay then send a message.
        std::thread::sleep(Duration::from_millis(150));
        send(&root);

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

    // --- busy parsing ----------------------------------------------------

    #[test]
    fn parse_busy_running_background_task() {
        let json = r#"{"background_tasks":[{"status":"running"}],"session_crons":[]}"#;
        assert!(parse_busy(json));
    }

    #[test]
    fn parse_busy_background_task_without_status_is_not_busy() {
        // Only a "running" background task counts; bare presence does not.
        let json = r#"{"background_tasks":[{"id":"bash_1"}],"session_crons":[]}"#;
        assert!(!parse_busy(json));
    }

    #[test]
    fn parse_busy_pending_background_task_is_not_busy() {
        // Non-running statuses do not count as busy for background tasks.
        let json = r#"{"background_tasks":[{"status":"pending"}],"session_crons":[]}"#;
        assert!(!parse_busy(json));
    }

    #[test]
    fn parse_busy_completed_background_task_is_not_busy() {
        // Terminal status → not running, so the never-idle message loop resumes
        // once the task has finished.
        let json = r#"{"background_tasks":[{"status":"completed"}],"session_crons":[]}"#;
        assert!(!parse_busy(json));
    }

    #[test]
    fn parse_busy_mixed_background_tasks() {
        let json = r#"{"background_tasks":[{"status":"completed"},{"status":"running"}],"session_crons":[]}"#;
        assert!(parse_busy(json));
    }

    #[test]
    fn parse_busy_active_cron() {
        let json = r#"{"background_tasks":[],"session_crons":[{"status":"active"}]}"#;
        assert!(parse_busy(json));
    }

    #[test]
    fn parse_busy_cron_without_status_is_busy() {
        // Presence with no status → treat as active.
        let json = r#"{"background_tasks":[],"session_crons":[{"id":"c1"}]}"#;
        assert!(parse_busy(json));
    }

    #[test]
    fn parse_busy_terminal_cron_is_not_busy() {
        let json = r#"{"background_tasks":[],"session_crons":[{"status":"completed"}]}"#;
        assert!(!parse_busy(json));
    }

    #[test]
    fn parse_busy_empty_arrays_is_not_busy() {
        let json = r#"{"background_tasks":[],"session_crons":[]}"#;
        assert!(!parse_busy(json));
    }

    #[test]
    fn parse_busy_missing_fields_is_not_busy() {
        let json = r#"{"session_id":"abc","hook_event_name":"Stop"}"#;
        assert!(!parse_busy(json));
    }

    #[test]
    fn parse_busy_empty_stdin_is_not_busy() {
        assert!(!parse_busy(""));
    }

    #[test]
    fn parse_busy_malformed_json_is_not_busy() {
        assert!(!parse_busy("not json at all"));
    }

    #[test]
    fn parse_busy_non_array_fields_is_not_busy() {
        // Defensive: non-array values must not panic, just mean "not busy".
        let json = r#"{"background_tasks":"oops","session_crons":42}"#;
        assert!(!parse_busy(json));
    }
}
