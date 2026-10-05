//! `pm harness hooks stop` — the Stop hook that keeps pm agents never-idle.
//!
//! Decision: queued messages → `block`; else a running background task or
//! active cron → `{}` (let it stop, so the running work can finish); else
//! block on `agent_wait` until a message arrives. A yield request
//! ([`runtime`]), there as the hook starts or arriving while it waits,
//! also gets `{}` when no message is queued: Claude Code and codex hold
//! text typed while the hook runs until it returns, then submit it as the
//! user's prompt, which ends in this hook again. A request whose text the
//! conversation already holds — taken in mid-turn — is dropped instead. A
//! `block` takes the request too: the harness submits what it holds with
//! the continuation. `{}` is the documented
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
//! Two signals, either sufficient: our parent pid changes (the installed
//! command execs pm, so the harness is our parent and its death reparents
//! us), or the peer of our stdout closes (an install that predates the
//! `exec` leaves an intermediate `/bin/sh` as our parent, which is orphaned
//! instead, so the parent never changes; codex and the opencode plugin read
//! stdout through a pipe, Claude Code through a socketpair, and `poll`
//! reports a closed peer of either as `POLLHUP`/`POLLERR`). Neither fires
//! while the harness is alive, so a live agent's hook keeps blocking.
//!
//! The hook keeps the agent's waiting marker ([`runtime`]): it clears it as a
//! turn ends, writes `background` when it yields, and `hook-ended` when its
//! harness ends it or the wait fails — the agent then sits at its prompt
//! where no message wakes it. A hook whose harness is gone writes nothing:
//! the agent is dead, or already respawned, and a late marker would mark the
//! new session unarmed. Claude Code ends the hook with SIGTERM (on
//! its timeout, or Esc while it waits) and kills it outright soon after, so
//! the wait sleeps on a pipe the signal handler writes to and records the
//! marker at once. A SIGTERM from any process but the harness (our parent)
//! — a `pkill` matching hook command lines machine-wide — is ignored, so it
//! cannot leave the agent unarmed (`Caller::sent` has why only SIGTERM).
//! Codex kills the hook with an uncatchable signal and reports
//! the interrupt through its own hook instead; opencode's plugin kills it
//! the same way when a turn it did not prompt starts, and waits again once
//! that turn ends.

mod signals;

use std::io::Read;
use std::time::Duration;

use serde_json::json;

use crate::commands::attention::AgentState;
use crate::commands::{agent_input, agent_wait};
use crate::messages;
use crate::state::runtime::{self, Waiting, WaitingKind};
use crate::state::{agent as registry, paths};

use signals::{Caught, Signals};

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

/// Run the Stop hook. Prints its answer as JSON and returns the exit code,
/// always 0, like every hook handler's.
/// Non-pm sessions (unresolvable agent/scope) let the turn end, staying invisible.
/// `on_turn` is told the agent's state and unread count as it enters its
/// wait (idle), as it returns `block` (busy), as it yields (background) and
/// as it ends without a decision (unarmed); it must not block.
///
/// Ending without a decision while the harness is still there — it sent
/// the signal, or the wait failed — prints the reason as a `systemMessage`,
/// which Claude Code and codex both show the user (verified on 2.1.287 and
/// 0.160). A non-zero exit would not do: codex drops a failed hook's stderr,
/// and Claude Code feeds exit 2's back as a prompt, looping the agent.
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
                    let caught = signals.pause(interval);
                    if let Some(caught) = caught.iter().find(|c| caller.sent(c)) {
                        return Some(Ended::By(format!("ended by {caught}")));
                    }
                }
                None => std::thread::sleep(interval),
            }
            (!caller.alive()).then_some(Ended::HarnessGone)
        },
    );
    let why = match decided {
        Ok(Decided::Answer(json)) => {
            print!("{json}");
            return 0;
        }
        // The harness is gone, so there is no one to tell.
        Ok(Decided::Ended(Ended::HarnessGone)) => return 0,
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

    /// Whether `caught` should end the wait. A SIGTERM must come from the
    /// harness: it is what `kill`, `pkill` and `killall` send by default, so
    /// a stray one must not leave the agent unarmed. SIGINT and SIGHUP are
    /// what a terminal sends, and macOS reports a terminal's signal as sent
    /// by whichever process wrote to it, so they end the wait from anyone.
    fn sent(&self, caught: &Caught) -> bool {
        caught.signal != libc::SIGTERM || caught.sender == self.parent
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

/// Why a wait ended without a decision.
#[derive(Debug, PartialEq)]
enum Ended {
    /// The harness ended it; the reason completes "Stop hook …".
    By(String),
    /// The harness that ran the hook is gone.
    HarnessGone,
}

#[derive(Debug, PartialEq)]
enum Decided {
    /// The JSON to print.
    Answer(String),
    Ended(Ended),
}

/// Decide the Stop outcome. Testable seam: takes an explicit `busy` flag
/// instead of reading stdin. Messages take priority over `busy`. `pause`
/// waits up to the poll interval between checks, and returns why the wait
/// should end once it should; an end its harness caused is recorded.
fn wait_and_decide(
    busy: bool,
    project_root: &std::path::Path,
    feature: &str,
    agent: &str,
    poll_interval: Option<Duration>,
    on_turn: &mut dyn FnMut(AgentState, u32),
    mut pause: impl FnMut(Duration) -> Option<Ended>,
) -> crate::error::Result<Decided> {
    let block = |on_turn: &mut dyn FnMut(AgentState, u32), senders: &[String]| {
        let _ = runtime::touch_activity(project_root, feature, agent);
        // The harness submits text it holds with the continuation.
        let _ = runtime::take_yield_request(project_root, feature, agent);
        let unread = messages::unread_count(&paths::messages_dir(project_root), feature, agent);
        on_turn(AgentState::Busy, unread);
        Decided::Answer(block_decision(senders))
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
        return Ok(Decided::Answer(allow_decision()));
    }
    if let Some(answer) = yielded(project_root, feature, agent, on_turn)? {
        return Ok(answer);
    }
    let waited = loop {
        on_turn(AgentState::Idle, 0);
        let mut ended = None;
        let mut yield_requested = false;
        let waited =
            agent_wait::agent_wait_while(project_root, feature, agent, None, poll_interval, |d| {
                ended = pause(d);
                yield_requested =
                    ended.is_none() && runtime::yield_requested(project_root, feature, agent);
                ended.is_none() && !yield_requested
            })?;
        if !yield_requested {
            break waited.ok_or(ended);
        }
        if !unread_senders(project_root, feature, agent)?.is_empty() {
            break Ok(0);
        }
        if let Some(answer) = yielded(project_root, feature, agent, on_turn)? {
            return Ok(answer);
        }
    };
    if let Err(ended) = waited {
        let ended = ended.unwrap_or(Ended::HarnessGone);
        if let Ended::By(why) = &ended {
            hook_ended(project_root, feature, agent, why.clone(), on_turn);
        }
        return Ok(Decided::Ended(ended));
    }
    let senders = unread_senders(project_root, feature, agent)?;
    Ok(block(on_turn, &senders))
}

/// Take a yield request (`runtime::request_yield`), so the harness can
/// submit the text typed into it: the answer that lets the turn end, the
/// agent busy with the text. `None` when there is none, or its text is
/// already in the conversation — taken in mid-turn — so nothing is held.
fn yielded(
    project_root: &std::path::Path,
    feature: &str,
    agent: &str,
    on_turn: &mut dyn FnMut(AgentState, u32),
) -> crate::error::Result<Option<Decided>> {
    let Some(request) = runtime::take_yield_request(project_root, feature, agent)? else {
        return Ok(None);
    };
    // Best-effort: the request is real, so an unreadable conversation yields.
    let said = || -> crate::error::Result<bool> {
        let Some(after) = &request.after else {
            return Ok(false);
        };
        Ok(
            match registry::conversation(project_root, feature, agent)? {
                Some(conversation) => {
                    agent_input::said(&conversation, after, &request.text_sha256)?
                }
                None => false,
            },
        )
    };
    if said().unwrap_or(false) {
        return Ok(None);
    }
    on_turn(AgentState::Busy, 0);
    Ok(Some(Decided::Answer(allow_decision())))
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
            false,
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
        .answer();

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
        .answer();
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
            false,
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
            false,
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

    fn request(root: &std::path::Path, after: Option<String>) {
        let request = runtime::YieldRequest {
            text_sha256: agent_input::sha256("deploy it"),
            after,
        };
        runtime::request_yield(root, "login", "reviewer", &request).unwrap();
    }

    #[test]
    fn a_request_for_text_already_taken_in_mid_turn_is_dropped_and_the_hook_waits() {
        use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: "s1".into(),
                window_name: "reviewer".into(),
                active: true,
                agent_definition: None,
                harness: crate::harness::Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&paths::agents_dir(&root), "login").unwrap();
        let transcript = dir.path().join("s1.jsonl");
        let line = |uuid: &str, text: &str| {
            json!({"type": "user", "uuid": uuid, "promptSource": "typed",
                   "message": {"role": "user", "content": text}})
            .to_string()
                + "\n"
        };
        std::fs::write(&transcript, line("u1", "earlier")).unwrap();
        runtime::write_session_path(
            &root,
            "login",
            "reviewer",
            runtime::SessionPath::Transcript,
            Some(&transcript),
        )
        .unwrap();
        let after = registry::conversation(&root, "login", "reviewer")
            .unwrap()
            .unwrap()
            .page(None, 1)
            .unwrap()
            .after;
        request(&root, Some(after));
        let mut file = std::fs::File::options()
            .append(true)
            .open(&transcript)
            .unwrap();
        std::io::Write::write_all(&mut file, line("u2", "deploy it\n").as_bytes()).unwrap();
        let mut polls = 0;

        let result = wait_and_decide(
            false,
            &root,
            "login",
            "reviewer",
            Some(Duration::from_millis(10)),
            &mut |_, _| {},
            |_| {
                polls += 1;
                (polls > 3).then(|| Ended::By("ended by SIGTERM".to_string()))
            },
        )
        .unwrap();

        assert_eq!(result, Decided::Ended(Ended::By("ended by SIGTERM".into())));
        assert!(!runtime::yield_requested(&root, "login", "reviewer"));
    }

    #[test]
    fn a_request_yields_when_the_conversation_cannot_be_read() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        std::fs::create_dir_all(paths::agents_dir(&root)).unwrap();
        std::fs::write(paths::agents_dir(&root).join("login.toml"), "not = [toml").unwrap();
        request(&root, Some("0".into()));

        let result = wait_and_decide(
            false,
            &root,
            "login",
            "reviewer",
            Some(Duration::from_secs(30)),
            &mut |_, _| {},
            |_| None,
        )
        .unwrap()
        .answer();

        assert_eq!(result, "{}");
    }

    #[test]
    fn a_yield_request_lets_the_turn_end_with_the_agent_busy() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        request(&root, None);
        let mut turns = Vec::new();

        let result = wait_and_decide(
            false,
            &root,
            "login",
            "reviewer",
            Some(Duration::from_secs(30)),
            &mut |state, unread| turns.push((state, unread)),
            |_| None,
        )
        .unwrap()
        .answer();

        assert_eq!(result, "{}");
        assert_eq!(turns, [(AgentState::Busy, 0)]);
        assert!(!runtime::yield_requested(&root, "login", "reviewer"));
        assert_eq!(runtime::read_waiting(&root, "login", "reviewer"), None);
    }

    #[test]
    fn a_yield_requested_while_waiting_ends_the_wait() {
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
                if polls == 3 {
                    request(&root, None);
                }
                None
            },
        )
        .unwrap()
        .answer();

        assert_eq!(result, "{}");
        assert_eq!(turns, [(AgentState::Idle, 0), (AgentState::Busy, 0)]);
        assert!(!runtime::yield_requested(&root, "login", "reviewer"));
    }

    #[test]
    fn messages_take_priority_over_a_yield_request() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        send(&root);
        request(&root, None);

        let result = wait_and_decide(
            false,
            &root,
            "login",
            "reviewer",
            None,
            &mut |_, _| {},
            |_| None,
        )
        .unwrap()
        .answer();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "block");
        assert!(!runtime::yield_requested(&root, "login", "reviewer"));
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
