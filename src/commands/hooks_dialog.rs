//! `pm harness hooks dialog <harness>`: makes a dialog on an agent's screen
//! answerable from `pm serve`. Run where the harness waits on a decision
//! for the dialog — Claude Code's `PermissionRequest` hook, the opencode
//! plugin's handler for a permission ask — it records the dialog
//! ([`runtime::DialogRecord`]) and blocks until an answer is left for it,
//! then prints the harness's own decision for that answer. Never keys: the
//! answer goes back the way a local one would, through the harness.
//!
//! The terminal's dialog stays up while it waits, and the first answer
//! wins. It ends without printing anything once the dialog is no longer
//! up: the agent's waiting marker stops standing for it (answered or
//! rejected at the terminal, the turn ended, a newer dialog), a newer
//! dialog's record replaced its own, or its harness ended it or is gone
//! ([`hook_process`]). The marker is written by the waiting hook, which
//! runs beside this one, so it is looked for only once it has had time to
//! appear. Only the latest of several dialogs open at once is answerable.
//!
//! Its stdout is the decision, so it prints nothing else.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::commands::hook_process::{self, Caller, Signals};
use crate::commands::running_agents;
use crate::error::Result;
use crate::harness::Harness;
use crate::state::paths;
use crate::state::runtime::{self, Answer, DialogRecord};

/// How often the wait checks for an answer.
const POLL: Duration = Duration::from_millis(250);
/// How long the waiting marker may take to stand for the dialog.
const MARKER_GRACE: Duration = Duration::from_secs(5);

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
    if let Ok(Ended::Answered(decision)) = &ended {
        print!("{decision}");
    }
    if !matches!(ended, Ok(Ended::Replaced)) {
        let _ = runtime::remove_dialog(&project_root, &scope, &agent, &record.dialog.id);
        on_change();
    }
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
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let scope = paths::resolve_scope_from(&project_root, &cwd)?;
    let record = DialogRecord {
        dialog,
        pid: std::process::id(),
        reply_context,
    };
    runtime::write_dialog(&project_root, &scope, &agent, &record)?;
    Ok(Some((project_root, scope, agent, record)))
}

/// Why the wait ended.
#[derive(Debug, PartialEq)]
enum Ended {
    /// An answer arrived: the decision to print.
    Answered(serde_json::Value),
    /// A newer dialog's record replaced this one's.
    Replaced,
    /// The dialog is no longer up, or the harness ended the hook.
    Gone,
}

/// Wait for an answer to `record`'s dialog. `pause` sleeps up to the
/// interval between checks and returns whether the harness ended the hook.
fn wait(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    record: &DialogRecord,
    poll: Duration,
    mut pause: impl FnMut(Duration) -> bool,
) -> Result<Ended> {
    let id = &record.dialog.id;
    let start = Instant::now();
    let mut marked = false;
    loop {
        let current = runtime::read_dialog(project_root, scope, agent);
        if current.is_none_or(|r| &r.dialog.id != id) {
            return Ok(Ended::Replaced);
        }
        if let Some(answer) = runtime::take_answer(project_root, scope, agent)? {
            if &answer.id == id {
                return Ok(Ended::Answered(harness.dialog_decision(record, &answer)));
            }
            // A newer dialog's, written since its record replaced this one.
            if runtime::read_dialog(project_root, scope, agent)
                .is_some_and(|r| r.dialog.id == answer.id)
            {
                runtime::write_answer(project_root, scope, agent, &answer)?;
            }
        }
        let marks = marker_matches(project_root, scope, agent, harness, record);
        if marked && !marks || !marked && start.elapsed() > MARKER_GRACE {
            return Ok(Ended::Gone);
        }
        marked |= marks;
        if pause(poll) {
            return Ok(Ended::Gone);
        }
    }
}

/// Whether the agent's waiting marker stands for `record`'s dialog.
fn marker_matches(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    record: &DialogRecord,
) -> bool {
    running_agents::waiting(project_root, scope, agent, harness)
        .is_some_and(|w| w.kind == record.dialog.kind && w.subagent == record.dialog.subagent)
}

/// The agent's dialog that can be answered now: recorded, its hook alive,
/// and the waiting marker standing for it.
pub fn current(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
) -> Option<DialogRecord> {
    runtime::read_dialog(project_root, scope, agent).filter(|r| {
        hook_process::pid_alive(r.pid) && marker_matches(project_root, scope, agent, harness, r)
    })
}

/// What became of an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// The dialog's hook took it.
    Taken,
    /// The answer doesn't fit the dialog: why.
    Invalid(String),
    /// The dialog it names is no longer up: answered at the terminal, or
    /// replaced by a newer one.
    Elsewhere,
    /// The dialog's hook is gone, or never took the answer.
    Gone,
}

/// Leave `answer` for the agent's dialog and wait up to `within` for its
/// hook to take it.
pub fn answer(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    answer: &Answer,
    within: Duration,
) -> Result<Answered> {
    let Some(record) = runtime::read_dialog(project_root, scope, agent)
        .filter(|r| r.dialog.id == answer.id)
        .filter(|r| marker_matches(project_root, scope, agent, harness, r))
    else {
        return Ok(Answered::Elsewhere);
    };
    if !hook_process::pid_alive(record.pid) {
        return Ok(Answered::Gone);
    }
    if let Some(why) = record.dialog.invalid(answer) {
        return Ok(Answered::Invalid(why));
    }
    runtime::write_answer(project_root, scope, agent, answer)?;
    let deadline = Instant::now() + within;
    loop {
        let pending = runtime::answer_pending(project_root, scope, agent);
        let held = runtime::read_dialog(project_root, scope, agent)
            .is_some_and(|r| r.dialog.id == answer.id);
        if !pending && !held {
            return Ok(Answered::Taken);
        }
        if pending && (Instant::now() >= deadline || !hook_process::pid_alive(record.pid)) {
            // Withdrawn, unless the hook took it meanwhile.
            return Ok(match runtime::take_answer(project_root, scope, agent)? {
                Some(_) => Answered::Gone,
                None => Answered::Taken,
            });
        }
        if !pending && Instant::now() >= deadline {
            return Ok(Answered::Taken);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::runtime::{ANSWER_CHOICE, Waiting, WaitingKind};
    use serde_json::json;
    use tempfile::tempdir;

    const AGENT: &str = "implementer";

    fn question(root: &Path) -> DialogRecord {
        let payload = json!({
            "hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
            "tool_input": {"questions": [{"question": "Which DB?", "header": "DB",
                "multiSelect": false, "options": [{"label": "SQLite", "description": ""}]}]}
        });
        let (dialog, reply_context) = Harness::ClaudeCode.dialog(&payload).unwrap();
        let record = DialogRecord {
            dialog,
            pid: std::process::id(),
            reply_context,
        };
        runtime::write_dialog(root, "login", AGENT, &record).unwrap();
        record
    }

    fn mark(root: &Path, kind: WaitingKind) {
        runtime::write_waiting(root, "login", AGENT, &Waiting::now(kind, None)).unwrap();
    }

    fn run(root: &Path, record: &DialogRecord, mut each: impl FnMut(u32)) -> Ended {
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
        let record = question(root);
        mark(root, WaitingKind::Question);
        let answer = |id: &str| Answer {
            id: id.into(),
            choice: ANSWER_CHOICE.into(),
            answers: [("Which DB?".to_string(), vec!["SQLite".to_string()])].into(),
            message: None,
        };
        let ended = run(root, &record, |poll| {
            let id = if poll == 1 {
                "stale"
            } else {
                &record.dialog.id
            };
            runtime::write_answer(root, "login", AGENT, &answer(id)).unwrap();
        });
        let Ended::Answered(decision) = ended else {
            panic!("{ended:?}")
        };
        assert_eq!(
            decision["hookSpecificOutput"]["decision"]["updatedInput"]["answers"],
            json!({"Which DB?": "SQLite"})
        );
    }

    #[test]
    fn the_wait_ends_once_the_marker_stops_standing_for_the_dialog() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let record = question(root);
        mark(root, WaitingKind::Question);
        let ended = run(root, &record, |_| {
            runtime::clear_waiting(root, "login", AGENT).unwrap();
        });
        assert_eq!(ended, Ended::Gone);

        let record = question(root);
        mark(root, WaitingKind::Question);
        let ended = run(root, &record, |_| mark(root, WaitingKind::Plan));
        assert_eq!(ended, Ended::Gone, "another dialog's marker");

        let record = question(root);
        let ended = run(root, &record, |_| {
            question(root);
        });
        assert_eq!(ended, Ended::Replaced);
    }

    #[test]
    fn an_answer_is_refused_for_a_dialog_no_longer_up_or_one_it_does_not_fit() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let record = question(root);
        let reply = |choice: &str| Answer {
            id: record.dialog.id.clone(),
            choice: choice.into(),
            answers: Default::default(),
            message: None,
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
        assert_eq!(send(&reply("decline")), Answered::Elsewhere, "no marker");
        mark(root, WaitingKind::Question);
        assert!(matches!(send(&reply("maybe")), Answered::Invalid(_)));
        assert_eq!(
            send(&reply("decline")),
            Answered::Gone,
            "nothing takes it in time"
        );
        assert!(!runtime::answer_pending(root, "login", AGENT), "withdrawn");
        assert!(current(root, "login", AGENT, Harness::ClaudeCode).is_some());

        let mut dead = record.clone();
        dead.pid = u32::MAX / 2;
        runtime::write_dialog(root, "login", AGENT, &dead).unwrap();
        assert_eq!(send(&reply("decline")), Answered::Gone);
        assert!(current(root, "login", AGENT, Harness::ClaudeCode).is_none());
    }
}
