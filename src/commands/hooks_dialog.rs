//! `pm harness hooks dialog <harness>`: makes a dialog on an agent's screen
//! answerable from `pm serve`. Run where the harness waits on a decision
//! for the dialog — Claude Code's `PermissionRequest` hook, the opencode
//! plugin's handler for a permission ask — it records the dialog
//! ([`runtime::DialogRecord`]) and blocks until an answer is left for it,
//! then prints the harness's own decision for that answer. Never keys: the
//! answer goes back the way a local one would, through the harness.
//!
//! The terminal's dialog stays up while it waits, and the first answer
//! wins. Each dialog open at once has its own hook, so each is answerable,
//! including one the terminal queues behind another (Claude Code shows
//! parallel subagents' dialogs one at a time, and applies a decision for
//! one not yet shown). The hook ends without printing anything once its
//! dialog closes another way:
//!
//! - its harness ends it, or is gone ([`hook_process`]): Claude Code
//!   signals the hook of a dialog rejected or cancelled at the terminal,
//!   and opencode's plugin that of an ask settled at the TUI;
//! - the waiting hook closes its record, on the event that says it was
//!   answered at the terminal ([`resolve`]);
//! - for the agent's own dialog (not a subagent's), its turn has ended
//!   since it opened, which catches one answered at the terminal in a way
//!   no event ties back to it.
//!
//! Its stdout is the decision, so it prints nothing else.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::commands::hook_process::{self, Caller, Signals};
use crate::commands::{hooks_waiting, running_agents};
use crate::error::Result;
use crate::harness::Harness;
use crate::state::paths;
use crate::state::runtime::{self, Answer, DialogRecord, Waiting, WaitingClass};

/// How often the wait checks for an answer.
const POLL: Duration = Duration::from_millis(250);

/// Run the hook. `on_change` is told when the dialog becomes answerable
/// and when it stops being.
pub fn dialog(harness: Harness, mut on_change: impl FnMut()) -> i32 {
    let caller = Caller::current();
    let Ok(Some((project_root, scope, agent, record))) = open(harness) else {
        return 0;
    };
    on_change();
    let signals = Signals::install();
    let ended = wait(
        &project_root,
        &scope,
        &agent,
        harness,
        &record,
        POLL,
        |interval| {
            match &signals {
                Some(signals) => {
                    if signals.pause(interval).iter().any(|c| caller.sent(c)) {
                        return true;
                    }
                }
                None => std::thread::sleep(interval),
            }
            !caller.alive()
        },
    );
    if let Ok(Some(decision)) = &ended {
        print!("{decision}");
    }
    let taken = matches!(ended, Ok(Some(_)));
    let id = &record.dialog.id;
    if runtime::read_dialog(&project_root, &scope, &agent, id).is_some() {
        let _ = runtime::close_dialog(&project_root, &scope, &agent, id, taken);
        let _ =
            hooks_waiting::dialog_closed(&project_root, &scope, &agent, harness, &record.dialog);
    } else if taken {
        let _ = runtime::close_dialog(&project_root, &scope, &agent, id, taken);
    }
    on_change();
    0
}

type Opened = (std::path::PathBuf, String, String, DialogRecord);

fn open(harness: Harness) -> Result<Option<Opened>> {
    let Some(agent) = std::env::var("PM_AGENT_NAME")
        .ok()
        .filter(|a| !a.is_empty())
    else {
        return Ok(None);
    };
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&input) else {
        return Ok(None);
    };
    let Some((dialog, reply_context)) = harness.dialog(&payload) else {
        return Ok(None);
    };
    let (project_root, scope) = paths::agent_scope()?;
    let record = DialogRecord {
        dialog,
        pid: std::process::id(),
        reply_context,
    };
    runtime::write_dialog(&project_root, &scope, &agent, &record)?;
    Ok(Some((project_root, scope, agent, record)))
}

/// Wait for an answer to `record`'s dialog: the decision to print, or
/// `None` once the dialog closed another way. `pause` sleeps up to the
/// interval between checks and returns whether the harness ended the hook.
fn wait(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    record: &DialogRecord,
    poll: Duration,
    mut pause: impl FnMut(Duration) -> bool,
) -> Result<Option<serde_json::Value>> {
    let id = &record.dialog.id;
    loop {
        if let Some(answer) = runtime::take_answer(project_root, scope, agent, id)? {
            return Ok(Some(harness.dialog_decision(record, &answer)));
        }
        if runtime::read_dialog(project_root, scope, agent, id).is_none() {
            return Ok(None);
        }
        let waiting = running_agents::waiting(project_root, scope, agent, harness);
        if turn_ended(waiting.as_ref(), record) || pause(poll) {
            return Ok(None);
        }
    }
}

/// Whether `waiting` says the turn `record`'s dialog held up has ended.
fn turn_ended(waiting: Option<&Waiting>, record: &DialogRecord) -> bool {
    record.dialog.subagent.is_none()
        && waiting.is_some_and(|w| {
            w.kind.class() == WaitingClass::Unarmed && w.since > record.dialog.since
        })
}

/// Close each of the agent's dialogs that `payload`, from `pm harness
/// hooks waiting`, says was answered at the terminal.
pub fn resolve(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    payload: &serde_json::Value,
) -> Result<()> {
    for record in runtime::read_dialogs(project_root, scope, agent) {
        if harness.dialog_resolved(&record, payload) {
            runtime::close_dialog(project_root, scope, agent, &record.dialog.id, false)?;
        }
    }
    Ok(())
}

/// The agent's dialogs that can be answered now, oldest first: recorded,
/// their hooks alive, and not ended with the turn.
pub fn open_dialogs(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
) -> Vec<DialogRecord> {
    let records = runtime::read_dialogs(project_root, scope, agent);
    if records.is_empty() {
        return records;
    }
    let waiting = running_agents::waiting(project_root, scope, agent, harness);
    records
        .into_iter()
        .filter(|r| hook_process::pid_alive(r.pid) && !turn_ended(waiting.as_ref(), r))
        .collect()
}

/// The open dialog an agent already known to be at `waiting` is asking
/// about: the newest the marker stands for.
pub fn current_for(
    project_root: &Path,
    scope: &str,
    agent: &str,
    waiting: &Waiting,
) -> Option<DialogRecord> {
    runtime::read_dialogs(project_root, scope, agent)
        .into_iter()
        .rev()
        .find(|r| {
            waiting.kind == r.dialog.kind
                && waiting.subagent == r.dialog.subagent
                && hook_process::pid_alive(r.pid)
        })
}

/// What became of an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// The dialog's hook took it.
    Taken,
    /// The answer doesn't fit the dialog: why.
    Invalid(String),
    /// The dialog it names has closed: answered at the terminal or by
    /// another answer.
    Elsewhere,
    /// The dialog's hook is gone, or never took the answer.
    Gone,
    /// No dialog of the agent's ever had the id it names.
    Unknown,
}

/// Leave `answer` for the agent's dialog it names and wait up to `within`
/// for its hook to take it.
pub fn answer(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    answer: &Answer,
    within: Duration,
) -> Result<Answered> {
    let id = &answer.id;
    let Some(record) = runtime::read_dialog(project_root, scope, agent, id) else {
        return Ok(
            match runtime::dialog_closed(project_root, scope, agent, id) {
                true => Answered::Elsewhere,
                false => Answered::Unknown,
            },
        );
    };
    let waiting = running_agents::waiting(project_root, scope, agent, harness);
    if turn_ended(waiting.as_ref(), &record) {
        return Ok(Answered::Elsewhere);
    }
    if !hook_process::pid_alive(record.pid) {
        return Ok(Answered::Gone);
    }
    if let Some(why) = record.dialog.invalid(answer) {
        return Ok(Answered::Invalid(why));
    }
    if !runtime::leave_answer(project_root, scope, agent, answer)? {
        return Ok(Answered::Elsewhere);
    }
    let deadline = Instant::now() + within;
    loop {
        let pending = runtime::answer_pending(project_root, scope, agent, id);
        let held = runtime::read_dialog(project_root, scope, agent, id).is_some();
        let hook_alive = hook_process::pid_alive(record.pid);
        if !held && runtime::dialog_answer_taken(project_root, scope, agent, id) {
            return Ok(Answered::Taken);
        }
        // Closed another way, which dropped the answer, unless its hook
        // took it first and is about to say so.
        if !held && (!hook_alive || Instant::now() >= deadline) {
            return Ok(Answered::Elsewhere);
        }
        if pending && (Instant::now() >= deadline || !hook_alive) {
            // Withdrawn, unless the hook took it meanwhile.
            if runtime::take_answer(project_root, scope, agent, id)?.is_some() {
                return Ok(Answered::Gone);
            }
            let held = runtime::read_dialog(project_root, scope, agent, id).is_some();
            return Ok(
                match held || runtime::dialog_answer_taken(project_root, scope, agent, id) {
                    true => Answered::Taken,
                    false => Answered::Elsewhere,
                },
            );
        }
        if !pending && held && Instant::now() >= deadline {
            return Ok(Answered::Taken);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::runtime::{ANSWER_CHOICE, WaitingKind};
    use serde_json::json;
    use tempfile::tempdir;

    const AGENT: &str = "implementer";

    fn open_dialog(root: &Path, payload: serde_json::Value) -> DialogRecord {
        let (dialog, reply_context) = Harness::ClaudeCode.dialog(&payload).unwrap();
        let record = DialogRecord {
            dialog,
            pid: std::process::id(),
            reply_context,
        };
        runtime::write_dialog(root, "login", AGENT, &record).unwrap();
        record
    }

    fn question(root: &Path) -> DialogRecord {
        open_dialog(
            root,
            json!({
                "hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
                "tool_input": {"questions": [{"question": "Which DB?", "header": "DB",
                    "multiSelect": false, "options": [{"label": "SQLite", "description": ""}]}]}
            }),
        )
    }

    fn bash(root: &Path, subagent: &str, command: &str) -> DialogRecord {
        open_dialog(
            root,
            json!({"hook_event_name": "PermissionRequest", "agent_id": subagent,
                   "tool_name": "Bash", "tool_input": {"command": command}}),
        )
    }

    fn decline(id: &str) -> Answer {
        Answer {
            id: id.into(),
            choice: "decline".into(),
            answers: Default::default(),
            message: None,
        }
    }

    fn run(
        root: &Path,
        record: &DialogRecord,
        mut each: impl FnMut(u32),
    ) -> Option<serde_json::Value> {
        let mut polls = 0;
        wait(
            root,
            "login",
            AGENT,
            Harness::ClaudeCode,
            record,
            Duration::ZERO,
            |_| {
                polls += 1;
                assert!(polls < 1000, "never ended");
                each(polls);
                false
            },
        )
        .unwrap()
    }

    #[test]
    fn the_wait_ends_with_the_decision_for_its_own_answer_only() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let other = question(root);
        let record = question(root);
        let decision = run(root, &record, |poll| {
            let id = match poll {
                1 => &other.dialog.id,
                _ => &record.dialog.id,
            };
            let answer = Answer {
                id: id.clone(),
                choice: ANSWER_CHOICE.into(),
                answers: [("Which DB?".to_string(), vec!["SQLite".to_string()])].into(),
                message: None,
            };
            runtime::leave_answer(root, "login", AGENT, &answer).unwrap();
        })
        .unwrap();
        assert_eq!(
            decision["hookSpecificOutput"]["decision"]["updatedInput"]["answers"],
            json!({"Which DB?": "SQLite"})
        );
        assert!(
            runtime::answer_pending(root, "login", AGENT, &other.dialog.id),
            "left for the other dialog's hook"
        );
    }

    #[test]
    fn the_wait_ends_once_its_dialog_is_closed() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let record = question(root);
        let ended = run(root, &record, |_| {
            runtime::close_dialog(root, "login", AGENT, &record.dialog.id, false).unwrap();
        });
        assert_eq!(ended, None);
    }

    #[test]
    fn the_agents_own_dialog_ends_with_its_turn_and_a_subagents_outlives_it() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let own = question(root);
        let subagents = bash(root, "a1", "cargo test");
        let older_turn = Waiting {
            since: own.dialog.since - chrono::Duration::seconds(1),
            ..Waiting::now(WaitingKind::Interrupted, None)
        };
        runtime::write_waiting(root, "login", AGENT, &older_turn).unwrap();
        assert_eq!(
            open_dialogs(root, "login", AGENT, Harness::ClaudeCode).len(),
            2,
            "a turn end from before they opened"
        );
        runtime::write_waiting(
            root,
            "login",
            AGENT,
            &Waiting::now(WaitingKind::Interrupted, None),
        )
        .unwrap();
        let open = open_dialogs(root, "login", AGENT, Harness::ClaudeCode);
        assert_eq!(open, std::slice::from_ref(&subagents));
        assert_eq!(run(root, &own, |_| panic!("ended at once")), None);
        assert_eq!(
            answer(
                root,
                "login",
                AGENT,
                Harness::ClaudeCode,
                &decline(&own.dialog.id),
                Duration::ZERO
            )
            .unwrap(),
            Answered::Elsewhere
        );
    }

    #[test]
    fn a_tool_finishing_closes_its_own_dialog_and_leaves_the_others_open() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let a = bash(root, "a1", "touch a");
        let b = bash(root, "a2", "touch b");
        let mark = |r: &DialogRecord| {
            runtime::write_waiting(root, "login", AGENT, &r.dialog.waiting()).unwrap();
        };
        mark(&b);
        let marker = runtime::read_waiting(root, "login", AGENT).unwrap();
        assert_eq!(marker.describe(), "Bash: touch b");
        assert_eq!(
            current_for(root, "login", AGENT, &marker).map(|r| r.dialog.id),
            Some(b.dialog.id.clone())
        );

        let done = json!({"hook_event_name": "PostToolUse", "agent_id": "a2",
                          "tool_name": "Bash", "tool_input": {"command": "touch b"}});
        resolve(root, "login", AGENT, Harness::ClaudeCode, &done).unwrap();
        assert_eq!(
            open_dialogs(root, "login", AGENT, Harness::ClaudeCode),
            std::slice::from_ref(&a)
        );
        assert!(runtime::dialog_closed(root, "login", AGENT, &b.dialog.id));
    }

    #[test]
    fn an_answer_dropped_by_the_dialog_closing_another_way_is_not_reported_taken() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let race = |taken: bool| {
            let record = question(&root);
            let id = record.dialog.id.clone();
            let closer = {
                let (root, id) = (root.clone(), id.clone());
                std::thread::spawn(move || {
                    while !runtime::answer_pending(&root, "login", AGENT, &id) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    if taken {
                        runtime::take_answer(&root, "login", AGENT, &id).unwrap();
                    }
                    runtime::close_dialog(&root, "login", AGENT, &id, taken).unwrap();
                })
            };
            let answered = answer(
                &root,
                "login",
                AGENT,
                Harness::ClaudeCode,
                &decline(&id),
                Duration::from_millis(300),
            )
            .unwrap();
            closer.join().unwrap();
            answered
        };
        assert_eq!(race(false), Answered::Elsewhere, "rejected at the terminal");
        assert_eq!(race(true), Answered::Taken);
    }

    #[test]
    fn an_answer_is_refused_for_a_dialog_unknown_closed_or_gone_or_one_it_does_not_fit() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let record = question(root);
        let reply = |choice: &str| Answer {
            choice: choice.into(),
            ..decline(&record.dialog.id)
        };
        let send = |answer: &Answer| {
            super::answer(
                root,
                "login",
                AGENT,
                Harness::ClaudeCode,
                answer,
                Duration::from_millis(100),
            )
            .unwrap()
        };
        assert_eq!(send(&decline("0123abcd")), Answered::Unknown);
        assert!(matches!(send(&reply("maybe")), Answered::Invalid(_)));
        runtime::leave_answer(root, "login", AGENT, &reply("decline")).unwrap();
        assert_eq!(
            send(&reply("decline")),
            Answered::Elsewhere,
            "a second answer while the first waits"
        );
        runtime::take_answer(root, "login", AGENT, &record.dialog.id).unwrap();
        assert_eq!(
            send(&reply("decline")),
            Answered::Gone,
            "nothing takes it in time"
        );
        assert!(
            !runtime::answer_pending(root, "login", AGENT, &record.dialog.id),
            "withdrawn"
        );

        let mut dead = record.clone();
        dead.pid = u32::MAX / 2;
        runtime::write_dialog(root, "login", AGENT, &dead).unwrap();
        assert_eq!(send(&reply("decline")), Answered::Gone);
        assert!(open_dialogs(root, "login", AGENT, Harness::ClaudeCode).is_empty());

        runtime::close_dialog(root, "login", AGENT, &record.dialog.id, false).unwrap();
        assert_eq!(send(&reply("decline")), Answered::Elsewhere, "closed");
    }
}
