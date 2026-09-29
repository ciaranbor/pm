//! `pm harness hooks stop` — the Stop hook that keeps pm agents never-idle.
//!
//! Decision: queued messages → `block`; else a running background task or
//! active cron → `{}` (let it stop, so the running work can finish); else
//! block on `agent_wait` until a message arrives. `{}` is the documented
//! "allow" for Stop: a `decision` other than `block` fails schema validation.
//! Recurring crons stay active between fires, so an agent with one is
//! message-delivered only at fire boundaries. Codex's Stop payload carries
//! neither field and codex has no second wake source (a finished background
//! terminal does not wake the session), so `parse_busy` is false there and
//! codex agents block every turn.
//!
//! The wait ends without a decision once the harness that ran the hook is
//! gone, since a hook blocked in its wait outlives a harness that dies
//! without killing it: codex never kills it, and Claude Code kills the
//! hook's process group on a clean exit but not when it is SIGKILLed.
//! Two signals, either sufficient: our parent pid changes (codex runs the
//! hook as its direct child, so its death reparents us), or the peer of our
//! stdout closes (Claude Code runs it under an intermediate `/bin/sh`, which
//! is orphaned instead, so the parent never changes; codex and the opencode
//! plugin read stdout through a pipe, Claude Code through a socketpair, and
//! `poll` reports a closed peer of either as `POLLHUP`/`POLLERR`). Neither
//! fires while the harness is alive, so a live agent's hook keeps blocking.

use std::io::Read;
use std::time::Duration;

use serde_json::json;

use crate::commands::agent_wait;
use crate::messages;
use crate::state::paths;

/// Reason text returned after messages arrive; `senders` is oldest first.
fn reason(senders: &[String]) -> String {
    if senders.is_empty() {
        return "You have new messages. Run `pm msg read` to read them.".to_string();
    }
    format!(
        "You have new messages from {}. Run `pm msg read` to read them.",
        senders.join(", ")
    )
}

/// Run the Stop hook. Prints the decision JSON and returns the exit code.
/// Non-pm sessions (unresolvable agent/scope) let the turn end, staying invisible.
pub fn stop() -> i32 {
    match stop_inner() {
        Ok(Some(json)) => {
            print!("{json}");
            0
        }
        Ok(None) => 0,
        Err(_) => {
            print!("{}", allow_decision());
            0
        }
    }
}

/// `None` when the harness went away while the hook waited.
fn stop_inner() -> crate::error::Result<Option<String>> {
    let caller = Caller::current();
    // Resolve identity before reading stdin: non-pm sessions bail here, and
    // tests calling `stop_inner` without piped stdin must not block.
    let agent = std::env::var("PM_AGENT_NAME")
        .map_err(|_| crate::error::PmError::Messaging("no PM_AGENT_NAME".into()))?;

    let busy = read_busy_from_stdin();

    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let feature = paths::resolve_scope_from(&project_root, &cwd)?;

    wait_and_decide(busy, &project_root, &feature, &agent, None, || {
        caller.alive()
    })
}

/// The harness process that ran this hook, as it was when the hook started.
struct Caller {
    parent: libc::pid_t,
}

impl Caller {
    fn current() -> Self {
        // SAFETY: getppid() takes no arguments and cannot fail.
        Self {
            parent: unsafe { libc::getppid() },
        }
    }

    /// See the module docs for why both checks are needed.
    fn alive(&self) -> bool {
        // SAFETY: as above.
        let parent = unsafe { libc::getppid() };
        parent == self.parent && !peer_closed(libc::STDOUT_FILENO)
    }
}

/// Whether the reading end of `fd` — a pipe or socket — has been closed.
/// False for anything `poll` reports no hang-up on (a tty, a file,
/// `/dev/null`) and for a closed `fd`, so a hook run by hand keeps waiting.
fn peer_closed(fd: libc::c_int) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: one valid pollfd, count 1, zero timeout.
    let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
    ready > 0 && pfd.revents & (libc::POLLHUP | libc::POLLERR) != 0
}

/// Decide the Stop outcome. Testable seam: takes an explicit `busy` flag
/// instead of reading stdin. Messages take priority over `busy`. `None`
/// once `caller_alive` turns false while waiting.
fn wait_and_decide(
    busy: bool,
    project_root: &std::path::Path,
    feature: &str,
    agent: &str,
    poll_interval: Option<Duration>,
    caller_alive: impl Fn() -> bool,
) -> crate::error::Result<Option<String>> {
    let senders = unread_senders(project_root, feature, agent)?;
    if !senders.is_empty() {
        return Ok(Some(block_decision(&senders)));
    }
    if busy {
        return Ok(Some(allow_decision()));
    }
    let waited = agent_wait::agent_wait_while(
        project_root,
        feature,
        agent,
        None,
        poll_interval,
        caller_alive,
    )?;
    if waited.is_none() {
        return Ok(None);
    }
    let senders = unread_senders(project_root, feature, agent)?;
    Ok(Some(block_decision(&senders)))
}

/// Senders with unread messages, oldest first — the order bare `pm msg read`
/// will take them in.
fn unread_senders(
    project_root: &std::path::Path,
    feature: &str,
    agent: &str,
) -> crate::error::Result<Vec<String>> {
    let messages_dir = paths::messages_dir(project_root);
    Ok(
        messages::resolve_sender(&messages_dir, feature, agent, None)?
            .map(|c| std::iter::once(c.sender).chain(c.pending).collect())
            .unwrap_or_default(),
    )
}

fn block_decision(senders: &[String]) -> String {
    json!({"decision": "block", "reason": reason(senders)}).to_string()
}

fn allow_decision() -> String {
    json!({}).to_string()
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
            || true,
        )
        .unwrap()
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "block");
        assert_eq!(
            parsed["reason"],
            "You have new messages from implementer. Run `pm msg read` to read them."
        );
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
            || true,
        )
        .unwrap()
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "block");
        assert_eq!(
            parsed["reason"],
            "You have new messages from implementer. Run `pm msg read` to read them."
        );
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
            || true,
        )
        .unwrap()
        .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(result, "{}");
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
                || true,
            )
            .unwrap()
            .unwrap()
        });

        // Small delay then send a message.
        std::thread::sleep(Duration::from_millis(150));
        send(&root);

        let result = handle.join().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "block");
        assert_eq!(
            parsed["reason"],
            "You have new messages from implementer. Run `pm msg read` to read them."
        );
    }

    #[test]
    fn reason_names_pending_senders_oldest_first() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let mdir = paths::messages_dir(&root);
        for sender in ["zed", "amy"] {
            crate::messages::send(&mdir, "login", "reviewer", sender, "hi").unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }

        let result = wait_and_decide(
            false,
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(50)),
            || true,
        )
        .unwrap()
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed["reason"],
            "You have new messages from zed, amy. Run `pm msg read` to read them."
        );
    }

    #[test]
    fn idle_wait_ends_without_a_decision_once_the_caller_is_gone() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let polls = std::sync::atomic::AtomicU32::new(0);

        let result = wait_and_decide(
            false,
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(10)),
            || polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 3,
        )
        .unwrap();

        assert_eq!(result, None);
    }

    #[test]
    fn peer_closed_tracks_the_reading_end() {
        let mut fds = [0; 2];
        // SAFETY: fds has room for the two descriptors pipe() writes.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let [read, write] = fds;
        assert!(!peer_closed(write));
        // SAFETY: closing descriptors this test owns.
        unsafe { libc::close(read) };
        assert!(peer_closed(write));
        unsafe { libc::close(write) };
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
