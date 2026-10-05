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
//! `block` takes the request too: Claude Code submits what it holds with
//! the continuation, as codex does text sent with Enter (a steer); a codex
//! follow-up queued with Tab stays held across it. `{}` is the documented
//! "allow" for Stop: a `decision` other than `block` fails schema validation.
//! Recurring crons stay active between fires, so an agent with one is
//! message-delivered only at fire boundaries. Codex's Stop payload carries
//! neither field and codex has no second wake source (a finished background
//! terminal does not wake the session), so `parse_busy` is false there and
//! codex agents block every turn.
//!
//! Text typed straight into a codex window files no yield request, codex
//! running no hook as it holds it, so the hook watches for it itself
//! ([`Harness::watch_held_input`](crate::harness::Harness::watch_held_input)),
//! dropping what the conversation already holds as it drops a request.
//!
//! The wait ends without a decision once the harness that ran the hook is
//! gone ([`hook_process`](super::hook_process) has how that is told).
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

use std::io::Read;
use std::time::Duration;

use serde_json::json;

use crate::commands::attention::AgentState;
use crate::commands::{agent_input, agent_wait};
use crate::harness::{HeldInput, HeldText};
use crate::messages;
use crate::state::runtime::{self, Waiting, WaitingKind};
use crate::state::{agent as registry, paths};

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
    let payload = read_stdin();
    let busy = parse_busy(&payload);
    let Ok((project_root, scope)) = scope() else {
        print!("{}", allow_decision());
        return 0;
    };
    let mut held = watch_held_input(&project_root, &scope, &agent, &payload);
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
                        return Some(Woke::Ended(Ended::By(format!("ended by {caught}"))));
                    }
                }
                None => std::thread::sleep(interval),
            }
            if !caller.alive() {
                return Some(Woke::Ended(Ended::HarnessGone));
            }
            held.as_mut().and_then(HeldInput::arrived).map(Woke::Held)
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

/// The watch on input `agent`'s harness holds without telling pm, started
/// before the wait so input from before it does not count.
fn watch_held_input(
    project_root: &std::path::Path,
    scope: &str,
    agent: &str,
    payload: &str,
) -> Option<HeldInput> {
    let session_id = serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("session_id")?
        .as_str()?
        .to_string();
    let registry = registry::AgentRegistry::load(&paths::agents_dir(project_root), scope).ok()?;
    let harness = registry.get(agent)?.harness;
    harness.watch_held_input(&paths::home_dir().ok()?, &session_id)
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

/// Why a wait ended without a decision.
#[derive(Debug, PartialEq)]
enum Ended {
    /// The harness ended it; the reason completes "Stop hook …".
    By(String),
    /// The harness that ran the hook is gone.
    HarnessGone,
}

/// Why a pause cut the wait short.
#[derive(Debug, PartialEq)]
enum Woke {
    Ended(Ended),
    /// The harness has taken input while the hook waits that it reports
    /// through no hook.
    Held(HeldText),
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
    mut pause: impl FnMut(Duration) -> Option<Woke>,
) -> crate::error::Result<Decided> {
    let block = |on_turn: &mut dyn FnMut(AgentState, u32), senders: &[String]| {
        let _ = runtime::touch_activity(project_root, feature, agent);
        // What the harness holds goes in with the continuation (module doc).
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
        let mut held = None;
        let waited =
            agent_wait::agent_wait_while(project_root, feature, agent, None, poll_interval, |d| {
                match pause(d) {
                    Some(Woke::Ended(why)) => ended = Some(why),
                    Some(Woke::Held(text)) => held = Some(text),
                    None => {
                        yield_requested = runtime::yield_requested(project_root, feature, agent)
                    }
                }
                ended.is_none() && held.is_none() && !yield_requested
            })?;
        if held.is_none() && !yield_requested {
            break waited.ok_or(ended);
        }
        if !unread_senders(project_root, feature, agent)?.is_empty() {
            break Ok(0);
        }
        if let Some(held) = held {
            if !taken(project_root, feature, agent, &held) {
                let _ = runtime::take_yield_request(project_root, feature, agent);
                on_turn(AgentState::Busy, 0);
                return Ok(Decided::Answer(allow_decision()));
            }
            continue;
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

/// Whether `agent`'s conversation already holds `held`: taken in mid-turn,
/// its report arriving after the turn ended. Best-effort: an unreadable
/// conversation holds nothing, so the hook yields.
fn taken(project_root: &std::path::Path, feature: &str, agent: &str, held: &HeldText) -> bool {
    let Ok(Some(conversation)) = registry::conversation(project_root, feature, agent) else {
        return false;
    };
    conversation
        .page(None, TAKEN_LOOKBACK)
        .is_ok_and(|page| agent_input::said_since(&page.items, held.at, &held.text))
}

/// How many of the conversation's latest items [`taken`] searches; a steer
/// reported late was taken at the very end of the turn.
const TAKEN_LOOKBACK: usize = 50;

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
                (polls > 3).then(|| Woke::Ended(Ended::By("ended by SIGTERM".to_string())))
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
                Some(Woke::Ended(Ended::HarnessGone))
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

    /// Register `reviewer` with a Claude Code transcript holding `lines`,
    /// next to the project; returns its path.
    fn with_transcript(root: &std::path::Path, lines: &str) -> std::path::PathBuf {
        use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
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
        registry.save(&paths::agents_dir(root), "login").unwrap();
        let transcript = root.join("s1.jsonl");
        std::fs::write(&transcript, lines).unwrap();
        runtime::write_session_path(
            root,
            "login",
            "reviewer",
            runtime::SessionPath::Transcript,
            Some(&transcript),
        )
        .unwrap();
        transcript
    }

    fn held(text: &str, at: chrono::DateTime<chrono::Utc>) -> Woke {
        Woke::Held(HeldText {
            text: text.into(),
            at,
        })
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
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let line = |uuid: &str, text: &str| {
            json!({"type": "user", "uuid": uuid, "promptSource": "typed",
                   "message": {"role": "user", "content": text}})
            .to_string()
                + "\n"
        };
        let transcript = with_transcript(&root, &line("u1", "earlier"));
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
                (polls > 3).then(|| Woke::Ended(Ended::By("ended by SIGTERM".to_string())))
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
    fn input_held_while_waiting_ends_the_wait_with_the_agent_busy() {
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
                (polls == 3).then(|| held("deploy it", chrono::Utc::now()))
            },
        )
        .unwrap()
        .answer();

        assert_eq!(result, "{}");
        assert_eq!(turns, [(AgentState::Idle, 0), (AgentState::Busy, 0)]);
    }

    #[test]
    fn held_input_the_conversation_took_in_mid_turn_is_dropped_and_the_hook_waits() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let submitted = chrono::Utc::now() - chrono::Duration::seconds(5);
        let taken = json!({"type": "user", "uuid": "u1", "promptSource": "typed",
            "timestamp": (submitted + chrono::Duration::milliseconds(300)).to_rfc3339(),
            "message": {"role": "user", "content": "deploy it"}});
        with_transcript(&root, &format!("{taken}\n"));
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
                match polls {
                    2 => Some(held("deploy it", submitted)),
                    p if p > 4 => Some(Woke::Ended(Ended::By("ended by SIGTERM".into()))),
                    _ => None,
                }
            },
        )
        .unwrap();

        assert_eq!(result, Decided::Ended(Ended::By("ended by SIGTERM".into())));
    }

    #[test]
    fn held_input_said_before_it_was_submitted_still_yields() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let earlier = json!({"type": "user", "uuid": "u1", "promptSource": "typed",
            "timestamp": (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339(),
            "message": {"role": "user", "content": "deploy it"}});
        with_transcript(&root, &format!("{earlier}\n"));
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
                (polls == 2).then(|| held("deploy it", chrono::Utc::now()))
            },
        )
        .unwrap()
        .answer();

        assert_eq!(result, "{}");
    }

    #[test]
    fn messages_take_priority_over_held_input() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
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
                if polls == 3 {
                    send(&root);
                }
                (polls == 3).then(|| held("deploy it", chrono::Utc::now()))
            },
        )
        .unwrap()
        .answer();

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["decision"], "block");
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
