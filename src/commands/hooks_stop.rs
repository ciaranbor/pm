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
//!
//! The hook keeps the agent's waiting marker ([`runtime`]): it clears it as a
//! turn ends, writes `background` when it yields, and `hook-ended` whenever
//! it ends without a decision or fails — the agent then sits at its prompt
//! where no message wakes it. Claude Code ends the hook with SIGTERM (on
//! its timeout, or Esc while it waits) and kills it outright soon after, so
//! the wait sleeps on a pipe the signal handler writes to and records the
//! marker at once. Codex kills it with an uncatchable signal and reports
//! the interrupt through its own hook instead.

use std::io::Read;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use serde_json::json;

use crate::commands::agent_wait;
use crate::commands::attention::AgentState;
use crate::messages;
use crate::state::paths;
use crate::state::runtime::{self, Waiting, WaitingKind};

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
/// `on_turn` is told the agent's state and unread count as it enters its
/// wait (idle), as it returns `block` (busy), as it yields (background) and
/// as it ends without a decision (unarmed); it must not block.
pub fn stop(on_turn: &mut dyn FnMut(AgentState, u32)) -> i32 {
    let caller = Caller::current();
    // Resolve identity before reading stdin: a non-pm session bails here.
    let Ok(agent) = std::env::var("PM_AGENT_NAME") else {
        print!("{}", allow_decision());
        return 0;
    };
    let busy = read_busy_from_stdin();
    let Ok((project_root, scope)) = scope() else {
        print!("{}", allow_decision());
        return 0;
    };
    let signals = Signals::install();
    let decided = wait_and_decide(
        busy,
        &project_root,
        &scope,
        &agent,
        None,
        on_turn,
        |interval| {
            match &signals {
                Some(signals) => {
                    if let Some(signal) = signals.pause(interval) {
                        return Some(format!("ended by {signal}"));
                    }
                }
                None => std::thread::sleep(interval),
            }
            (!caller.alive()).then(|| "ended: its harness stopped waiting".to_string())
        },
    );
    match decided {
        Ok(Some(json)) => print!("{json}"),
        Ok(None) => {}
        Err(e) => {
            hook_ended(
                &project_root,
                &scope,
                &agent,
                format!("failed: {e}"),
                on_turn,
            );
            print!("{}", allow_decision());
        }
    }
    0
}

fn scope() -> crate::error::Result<(std::path::PathBuf, String)> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let scope = paths::resolve_scope_from(&project_root, &cwd)?;
    Ok((project_root, scope))
}

/// Record that the hook ended without the agent getting a decision, so it
/// sits at its prompt with nothing to wake it. Best-effort.
fn hook_ended(
    project_root: &std::path::Path,
    scope: &str,
    agent: &str,
    why: String,
    on_turn: &mut dyn FnMut(AgentState, u32),
) {
    let waiting = Waiting::now(WaitingKind::HookEnded, Some(format!("Stop hook {why}")));
    if runtime::write_waiting(project_root, scope, agent, &waiting).is_ok() {
        let unread = messages::unread_count(&paths::messages_dir(project_root), scope, agent);
        on_turn(AgentState::Unarmed, unread);
    }
}

/// SIGTERM, SIGHUP and SIGINT, caught for the rest of the process so the
/// wait can record why it ended before it exits. A handler may only do
/// async-signal-safe work, so it writes a byte to a pipe the wait polls.
struct Signals {
    read: libc::c_int,
    write: libc::c_int,
}

impl Drop for Signals {
    fn drop(&mut self) {
        Self::handle(libc::SIG_DFL);
        SIGNAL_PIPE.store(-1, Ordering::SeqCst);
        // SAFETY: closing the pipe this value opened.
        unsafe {
            libc::close(self.read);
            libc::close(self.write);
        }
    }
}

static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);
static CAUGHT: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(signal: libc::c_int) {
    CAUGHT.store(signal, Ordering::SeqCst);
    let fd = SIGNAL_PIPE.load(Ordering::SeqCst);
    if fd >= 0 {
        // SAFETY: write(2) is async-signal-safe; one byte from a static.
        unsafe { libc::write(fd, b"!".as_ptr().cast(), 1) };
    }
}

impl Signals {
    const CAUGHT: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

    /// `None` when the handlers can't be installed; the default actions
    /// then stay.
    fn install() -> Option<Self> {
        let mut fds = [0; 2];
        // SAFETY: fds has room for the two descriptors pipe() writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        let [read, write] = fds;
        // SAFETY: setting flags on descriptors this process just opened.
        unsafe {
            libc::fcntl(write, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(read, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(write, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        CAUGHT.store(0, Ordering::SeqCst);
        SIGNAL_PIPE.store(write, Ordering::SeqCst);
        Self::handle(on_signal as *const () as libc::sighandler_t);
        Some(Self { read, write })
    }

    fn handle(handler: libc::sighandler_t) {
        for signal in Self::CAUGHT {
            // SAFETY: a zeroed sigaction with either the default action or
            // an `extern "C"` handler that only touches atomics and write(2).
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = handler;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
    }

    /// Sleep up to `timeout`, or until a signal is caught. The signal's
    /// name once one has been.
    fn pause(&self, timeout: Duration) -> Option<&'static str> {
        let mut pfd = libc::pollfd {
            fd: self.read,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: one valid pollfd, count 1.
        unsafe { libc::poll(&mut pfd, 1, ms) };
        match CAUGHT.load(Ordering::SeqCst) {
            0 => None,
            libc::SIGTERM => Some("SIGTERM"),
            libc::SIGHUP => Some("SIGHUP"),
            _ => Some("SIGINT"),
        }
    }
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
/// instead of reading stdin. Messages take priority over `busy`. `pause`
/// waits up to the poll interval between checks, and returns why the wait
/// should end once it should; the hook then records that and returns
/// `None`.
fn wait_and_decide(
    busy: bool,
    project_root: &std::path::Path,
    feature: &str,
    agent: &str,
    poll_interval: Option<Duration>,
    on_turn: &mut dyn FnMut(AgentState, u32),
    mut pause: impl FnMut(Duration) -> Option<String>,
) -> crate::error::Result<Option<String>> {
    let block = |on_turn: &mut dyn FnMut(AgentState, u32), senders: &[String]| {
        let _ = runtime::touch_activity(project_root, feature, agent);
        let unread = messages::unread_count(&paths::messages_dir(project_root), feature, agent);
        on_turn(AgentState::Busy, unread);
        Some(block_decision(senders))
    };
    runtime::touch_activity(project_root, feature, agent)?;
    runtime::clear_waiting(project_root, feature, agent)?;
    let senders = unread_senders(project_root, feature, agent)?;
    if !senders.is_empty() {
        return Ok(block(on_turn, &senders));
    }
    if busy {
        let waiting = Waiting::now(WaitingKind::Background, None);
        runtime::write_waiting(project_root, feature, agent, &waiting)?;
        on_turn(AgentState::Background, 0);
        return Ok(Some(allow_decision()));
    }
    on_turn(AgentState::Idle, 0);
    let mut ended = None;
    let waited =
        agent_wait::agent_wait_while(project_root, feature, agent, None, poll_interval, |d| {
            ended = pause(d);
            ended.is_none()
        })?;
    if waited.is_none() {
        let why = ended.unwrap_or_default();
        hook_ended(project_root, feature, agent, why, on_turn);
        return Ok(None);
    }
    let senders = unread_senders(project_root, feature, agent)?;
    Ok(block(on_turn, &senders))
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
        send(&root);

        let mut turns = Vec::new();
        let result = wait_and_decide(
            false,
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(50)),
            &mut |state, unread| turns.push((state, unread)),
            |_| None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(turns, [(AgentState::Busy, 2)], "no wait, so never idle");

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
            &mut |_, _| {},
            |_| None,
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
            &mut |_, _| {},
            |_| None,
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
            let mut turns = Vec::new();
            let decision = wait_and_decide(
                false,
                &root_clone,
                "login",
                "reviewer",
                Some(Duration::from_millis(50)),
                &mut |state, unread| turns.push((state, unread)),
                |_| None,
            )
            .unwrap()
            .unwrap();
            (decision, turns)
        });

        // Small delay then send a message.
        std::thread::sleep(Duration::from_millis(150));
        send(&root);

        let (result, turns) = handle.join().unwrap();
        assert_eq!(turns, [(AgentState::Idle, 0), (AgentState::Busy, 1)]);
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
            &mut |_, _| {},
            |_| None,
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
    fn a_wait_that_ends_undecided_leaves_the_agent_unarmed() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let mut polls = 0;
        let mut turns = Vec::new();

        let result = wait_and_decide(
            false,
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(10)),
            &mut |state, unread| turns.push((state, unread)),
            |_| {
                polls += 1;
                (polls > 3).then(|| "ended by SIGTERM".to_string())
            },
        )
        .unwrap();

        assert_eq!(result, None);
        assert_eq!(turns, [(AgentState::Idle, 0), (AgentState::Unarmed, 0)]);
        let waiting = runtime::read_waiting(&root, "login", "reviewer").unwrap();
        assert_eq!(waiting.kind, WaitingKind::HookEnded);
        assert_eq!(waiting.describe(), "Stop hook ended by SIGTERM");
    }

    #[test]
    fn a_turn_end_clears_the_marker_and_a_yield_marks_background_work() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let asked = Waiting::now(WaitingKind::Question, Some("Which DB?".into()));
        runtime::write_waiting(&root, "login", "reviewer", &asked).unwrap();
        send(&root);

        wait_and_decide(
            false,
            &root,
            "login",
            "reviewer",
            None,
            &mut |_, _| {},
            |_| None,
        )
        .unwrap();
        assert_eq!(runtime::read_waiting(&root, "login", "reviewer"), None);
        assert!(runtime::last_activity(&root, "login", "reviewer").is_some());

        let mut turns = Vec::new();
        wait_and_decide(
            true,
            &root,
            "login",
            "qa",
            None,
            &mut |state, unread| turns.push((state, unread)),
            |_| None,
        )
        .unwrap();
        assert_eq!(turns, [(AgentState::Background, 0)]);
        assert_eq!(
            runtime::read_waiting(&root, "login", "qa").map(|w| w.kind),
            Some(WaitingKind::Background)
        );
    }

    #[test]
    fn a_caught_signal_ends_the_pause_at_once() {
        let signals = Signals::install().unwrap();
        assert_eq!(signals.pause(Duration::from_millis(1)), None);
        let start = Instant::now();
        let sender = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
            // SAFETY: raising a signal this process now handles.
            unsafe { libc::raise(libc::SIGHUP) };
        });
        assert_eq!(signals.pause(Duration::from_secs(30)), Some("SIGHUP"));
        assert!(start.elapsed() < Duration::from_secs(5));
        sender.join().unwrap();
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
