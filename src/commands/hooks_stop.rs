//! `pm harness hooks stop [harness]` — the Stop hook that keeps pm agents
//! never-idle.
//!
//! Named with a harness that runs it in the background once the turn has
//! ended ([`Harness::wake`]), it is that harness's waiter (`wake.rs` has
//! how it waits and wakes). Without one it is the waiter opencode's plugin
//! runs between turns: it answers `block` at once when messages are
//! queued, else waits on the inbox until one arrives. An entry an earlier
//! release installed for Claude Code or codex runs it too, inside the
//! turn, until `pm harness hooks install` replaces it (`pm doctor` reports
//! one).
//!
//! The wait ends without a decision once the harness that ran the hook is
//! gone ([`hook_process`](super::hook_process) has how that is told), or a
//! newer waiter took the agent over.
//!
//! The hook keeps the agent's waiting marker and waiter file ([`runtime`]):
//! it clears the marker as a turn ends, marks the agent idle as it waits,
//! clears that as it answers, and writes `hook-ended` when its harness
//! ends it or the wait fails — the agent then sits at its prompt where no
//! message wakes it. A hook whose harness is gone, or that was superseded,
//! writes nothing: the agent is dead, or already respawned, and a late
//! marker would mark the new session unarmed. Ended with SIGTERM, it
//! records the marker at once: the wait sleeps on a pipe the signal
//! handler writes to. A SIGTERM from any process but the harness (our
//! parent) — a `pkill` matching hook command lines machine-wide — is
//! ignored, so it cannot leave the agent unarmed (`Caller::sent` has why
//! only SIGTERM). opencode's plugin kills the hook with an uncatchable
//! signal when a turn it did not prompt starts, and waits again once that
//! turn ends.

mod wake;

pub(crate) use wake::is_loop_notice;

use std::io::Read;
use std::time::Duration;

use serde_json::json;

use crate::commands::agent_wait;
use crate::commands::attention::AgentState;
use crate::harness::{Harness, Wake};
use crate::messages;
use crate::state::paths;
use crate::state::runtime::{self, Waiting, WaitingKind};

use crate::commands::hook_process::{Caller, Signals};

const REASON_START: &str = "You have new messages";
const REASON_END: &str = ". Run `pm msg read` to read them.";

/// Reason text returned after messages arrive; `senders` is oldest first.
fn reason(senders: &[String]) -> String {
    if senders.is_empty() {
        return format!("{REASON_START}{REASON_END}");
    }
    format!("{REASON_START} from {}{REASON_END}", senders.join(", "))
}

/// The prompt that tells `agent` to read its unread messages: the reason
/// the hook returns, so pm sends one text however it reaches the agent.
pub(crate) fn continuation(
    project_root: &std::path::Path,
    scope: &str,
    agent: &str,
) -> crate::error::Result<String> {
    Ok(reason(&unread_senders(project_root, scope, agent)?))
}

/// Whether `prompt` is one pm generated with [`continuation`].
pub(crate) fn is_continuation(prompt: &str) -> bool {
    let prompt = prompt.trim();
    prompt == reason(&[])
        || prompt
            .strip_prefix(REASON_START)
            .and_then(|rest| rest.strip_suffix(REASON_END))
            .and_then(|rest| rest.strip_prefix(" from "))
            .is_some_and(|senders| !senders.is_empty() && !senders.contains('\n'))
}

/// Run the Stop hook: as `harness`'s waiter when its hook runs once the
/// turn has ended, else blocking. Returns the exit code.
/// Non-pm sessions (unresolvable agent/scope) let the turn end, staying invisible.
/// `on_turn` is told the agent's state and unread count as it enters its
/// wait (idle or background), as it delivers (busy), as it yields
/// (background) and as it ends without a decision (unarmed); it must not
/// block.
pub fn stop(harness: Option<Harness>, on_turn: &mut dyn FnMut(AgentState, u32)) -> i32 {
    match harness.filter(|h| h.wake() != Wake::Block) {
        Some(harness) => wake::run(harness, on_turn),
        None => blocking(on_turn),
    }
}

/// The blocking hook. Prints its answer as JSON and returns 0. Ending
/// without a decision while the harness is still there — it sent the
/// signal, or the wait failed — prints the reason as a `systemMessage`,
/// which opencode's plugin reports as the hook's failure.
fn blocking(on_turn: &mut dyn FnMut(AgentState, u32)) -> i32 {
    let caller = Caller::current();
    // Resolve identity before reading stdin: a non-pm session bails here.
    let Ok(agent) = std::env::var("PM_AGENT_NAME") else {
        print!("{}", allow_decision());
        return 0;
    };
    let _ = read_stdin();
    let Ok((project_root, scope)) = paths::agent_scope() else {
        print!("{}", allow_decision());
        return 0;
    };
    let log = |line: &str| runtime::log_stop_hook(&project_root, &scope, &agent, line);
    log("blocking hook");
    let signals = Signals::install();
    let decided = wait_and_decide(&project_root, &scope, &agent, None, on_turn, |interval| {
        match &signals {
            Some(signals) => {
                let caught = signals.pause(interval);
                if let Some(caught) = caught.iter().find(|c| caller.sent(c)) {
                    return Some(Ended::By(format!("ended by {caught}")));
                }
            }
            None => std::thread::sleep(interval),
        }
        (!caller.alive()).then_some(Ended::HarnessGone)
    });
    match &decided {
        Ok(Decided::Answer(json)) => log(&format!("answered {json}")),
        Ok(Decided::Ended(ended)) => log(&format!("ended undecided: {ended:?}")),
        Err(e) => log(&format!("failed: {e}")),
    }
    let why = match decided {
        Ok(Decided::Answer(json)) => {
            print!("{json}");
            return 0;
        }
        // The harness is gone, or a newer waiter tells it.
        Ok(Decided::Ended(Ended::HarnessGone | Ended::Superseded)) => return 0,
        Ok(Decided::Ended(Ended::By(why))) => why,
        Err(e) => {
            let why = format!("failed: {e}");
            hook_ended(&project_root, &scope, &agent, why.clone(), on_turn);
            why
        }
    };
    let message = format!("pm: Stop hook {why}; this agent is unarmed");
    print!("{}", json!({ "systemMessage": message }));
    0
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

/// Why a wait ended without a decision.
#[derive(Debug, PartialEq)]
enum Ended {
    /// The harness ended it; the reason completes "Stop hook …".
    By(String),
    /// The harness that ran the hook is gone.
    HarnessGone,
    /// A newer waiter took the agent over.
    Superseded,
}

#[derive(Debug, PartialEq)]
enum Decided {
    /// The JSON to print.
    Answer(String),
    Ended(Ended),
}

/// Decide the Stop outcome. `pause` waits up to the poll interval between
/// checks, and returns why the wait should end once it should; an end its
/// harness caused is recorded.
fn wait_and_decide(
    project_root: &std::path::Path,
    feature: &str,
    agent: &str,
    poll_interval: Option<Duration>,
    on_turn: &mut dyn FnMut(AgentState, u32),
    mut pause: impl FnMut(Duration) -> Option<Ended>,
) -> crate::error::Result<Decided> {
    runtime::touch_activity(project_root, feature, agent)?;
    runtime::clear_waiting(project_root, feature, agent)?;
    if unread_senders(project_root, feature, agent)?.is_empty() {
        let waiter = std::process::id();
        runtime::take_waiter(project_root, feature, agent, waiter, None)?;
        let idle = Waiting::now(WaitingKind::Idle, None);
        runtime::write_waiting(project_root, feature, agent, &idle)?;
        on_turn(AgentState::Idle, 0);
        let mut ended = None;
        let waited =
            agent_wait::agent_wait_while(project_root, feature, agent, None, poll_interval, |d| {
                ended = pause(d).or_else(|| {
                    (runtime::read_waiter(project_root, feature, agent) != Some(waiter))
                        .then_some(Ended::Superseded)
                });
                ended.is_none()
            })?;
        if waited.is_none() {
            let ended = ended.unwrap_or(Ended::HarnessGone);
            if let Ended::By(why) = &ended {
                hook_ended(project_root, feature, agent, why.clone(), on_turn);
            }
            return Ok(Decided::Ended(ended));
        }
        runtime::clear_waiting_if(project_root, feature, agent, WaitingKind::Idle)?;
    }
    let _ = runtime::touch_activity(project_root, feature, agent);
    let unread = messages::unread_count(&paths::messages_dir(project_root), feature, agent);
    on_turn(AgentState::Busy, unread);
    let senders = unread_senders(project_root, feature, agent)?;
    Ok(Decided::Answer(block_decision(&senders)))
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

/// The Stop payload; empty when unreadable, which parses as not busy and
/// no session, so the hook never crashes or hangs on unexpected input.
fn read_stdin() -> String {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        input.clear();
    }
    input
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

/// The Stop payload's background tasks and session crons, each as
/// `id:status`, for the log: what a decision on `busy` saw.
fn background_work(json_str: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap_or_default();
    let list = |key: &str| -> String {
        let Some(items) = parsed.get(key).and_then(|v| v.as_array()) else {
            return "none".into();
        };
        let items: Vec<String> = items
            .iter()
            .map(|item| {
                let field = |name: &str| item.get(name).and_then(|v| v.as_str()).unwrap_or("?");
                format!("{}:{}", field("id"), field("status"))
            })
            .collect();
        format!("[{}]", items.join(", "))
    };
    format!(
        "busy={} background_tasks={} session_crons={}",
        parse_busy(json_str),
        list("background_tasks"),
        list("session_crons")
    )
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

    impl Decided {
        fn answer(self) -> String {
            match self {
                Decided::Answer(json) => json,
                ended => panic!("expected an answer, got {ended:?}"),
            }
        }
    }

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
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(50)),
            &mut |state, unread| turns.push((state, unread)),
            |_| None,
        )
        .unwrap()
        .answer();

        assert_eq!(turns, [(AgentState::Busy, 2)], "no wait, so never idle");

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "block");
        assert_eq!(
            parsed["reason"],
            "You have new messages from implementer. Run `pm msg read` to read them."
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
                &root_clone,
                "login",
                "reviewer",
                Some(Duration::from_millis(50)),
                &mut |state, unread| turns.push((state, unread)),
                |_| None,
            )
            .unwrap()
            .answer();
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
    fn a_waiting_hook_is_the_agents_idle_waiter_until_it_answers() {
        let dir = tempdir().unwrap();
        let root = Arc::new(setup_project(dir.path()));
        let waiting = Arc::clone(&root);
        let handle = std::thread::spawn(move || {
            wait_and_decide(
                &waiting,
                "login",
                "reviewer",
                Some(Duration::from_millis(10)),
                &mut |_, _| {},
                |_| None,
            )
            .unwrap()
        });
        let kind = || runtime::read_waiting(&root, "login", "reviewer").map(|w| w.kind);
        let deadline = Instant::now() + Duration::from_secs(5);
        while kind() != Some(WaitingKind::Idle) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(kind(), Some(WaitingKind::Idle));
        assert_eq!(
            runtime::read_waiter(&root, "login", "reviewer"),
            Some(std::process::id())
        );

        send(&root);
        handle.join().unwrap().answer();
        assert_eq!(kind(), None, "busy with the continuation");
    }

    #[test]
    fn a_superseded_waiter_ends_without_a_word() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let mut polls = 0;
        let mut turns = Vec::new();

        let result = wait_and_decide(
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(10)),
            &mut |state, unread| turns.push((state, unread)),
            |_| {
                polls += 1;
                if polls == 2 {
                    runtime::take_waiter(&root, "login", "reviewer", u32::MAX, None).unwrap();
                }
                None
            },
        )
        .unwrap();

        assert_eq!(result, Decided::Ended(Ended::Superseded));
        assert_eq!(turns, [(AgentState::Idle, 0)]);
        let kind = runtime::read_waiting(&root, "login", "reviewer").map(|w| w.kind);
        assert_eq!(kind, Some(WaitingKind::Idle), "the newer waiter's to keep");
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
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(50)),
            &mut |_, _| {},
            |_| None,
        )
        .unwrap()
        .answer();

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
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(10)),
            &mut |state, unread| turns.push((state, unread)),
            |_| {
                polls += 1;
                (polls > 3).then(|| Ended::By("ended by SIGTERM".to_string()))
            },
        )
        .unwrap();

        assert_eq!(result, Decided::Ended(Ended::By("ended by SIGTERM".into())));
        assert_eq!(turns, [(AgentState::Idle, 0), (AgentState::Unarmed, 0)]);
        let waiting = runtime::read_waiting(&root, "login", "reviewer").unwrap();
        assert_eq!(waiting.kind, WaitingKind::HookEnded);
        assert_eq!(waiting.describe(), "Stop hook ended by SIGTERM");
    }

    #[test]
    fn a_wait_whose_harness_is_gone_leaves_the_marker_alone() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let mut turns = Vec::new();

        let result = wait_and_decide(
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(10)),
            &mut |state, unread| turns.push((state, unread)),
            |_| {
                // A respawned session's hook marks it while the orphan waits.
                let asked = Waiting::now(WaitingKind::Question, Some("Which DB?".into()));
                runtime::write_waiting(&root, "login", "reviewer", &asked).unwrap();
                Some(Ended::HarnessGone)
            },
        )
        .unwrap();

        assert_eq!(result, Decided::Ended(Ended::HarnessGone));
        assert_eq!(turns, [(AgentState::Idle, 0)]);
        let waiting = runtime::read_waiting(&root, "login", "reviewer").unwrap();
        assert_eq!(waiting.kind, WaitingKind::Question);
    }

    #[test]
    fn a_turn_end_clears_the_marker() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let asked = Waiting::now(WaitingKind::Question, Some("Which DB?".into()));
        runtime::write_waiting(&root, "login", "reviewer", &asked).unwrap();
        send(&root);

        wait_and_decide(&root, "login", "reviewer", None, &mut |_, _| {}, |_| None).unwrap();
        assert_eq!(runtime::read_waiting(&root, "login", "reviewer"), None);
        assert!(runtime::last_activity(&root, "login", "reviewer").is_some());
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
