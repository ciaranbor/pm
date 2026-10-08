//! The waiter: pm's Stop hook on a harness that runs it in the background
//! once the turn has ended ([`Wake::Rewake`], [`Wake::Queue`]), so the
//! agent sits at its prompt between turns and takes typing and keys as
//! the user's. A SessionStart hook that
//! [waits](crate::harness::Harness::waits_at_session_start) runs it too, so
//! the agent waits from its start without a first turn.
//!
//! It takes over the agent's waiter file and marks the agent idle (or
//! background, while background work runs, which the waiter file records
//! too: a prompt clears the marker, and an interrupt can leave this waiter
//! the agent's only one; or asking, while a subagent's dialog is still
//! open), then waits on the inbox. With
//! messages unread it hands the continuation to the harness: at once when
//! some are unread as it starts. The harness never ends an earlier waiter
//! as a later turn starts, so one can outlive several turns and fire
//! mid-turn; each new one supersedes it, and a superseded waiter, or one
//! whose harness is gone, ends without a word or a marker. Whatever is
//! delivered twice — a mid-turn wake, a typed re-arm, a stale queue item —
//! UserPromptSubmit drops once the inbox is empty
//! ([`hooks_user_prompt`](crate::commands::hooks_user_prompt)).
//!
//! The breaker stops a loop that wakes the agent turn after turn without
//! the inbox draining: a wake that finds messages unread as it starts, the
//! agent having read none since the last wake, is wasted, and
//! [`MAX_WASTED`] in a row record the stopped loop ([`runtime::trip_loop`])
//! and wake the agent once more with a notice saying so. A stopped loop's
//! waiter ends at once until a spawn resets it. Speed alone does not count:
//! a sender that replies before the turn ends always has the next message
//! unread.

use std::path::Path;
use std::time::Duration;

use crate::commands::agent_wait;
use crate::commands::attention::AgentState;
use crate::commands::hook_process::Caller;
use crate::commands::{hooks_dialog, hooks_waiting};
use crate::error::Result;
use crate::harness::{Harness, Wake};
use crate::messages;
use crate::state::paths;
use crate::state::runtime::{self, Breaker, Waiting, WaitingKind};

use super::{background_work, hook_ended, parse_busy, read_stdin, reason, unread_senders};

/// Wasted wakes in a row that stop the loop.
pub(super) const MAX_WASTED: u32 = 5;

const NOTICE_START: &str = "pm: never-idle loop stopped: ";

/// Whether `prompt` is the notice of a loop that stopped itself.
pub(crate) fn is_loop_notice(prompt: &str) -> bool {
    prompt.trim_start().starts_with(NOTICE_START)
}

fn notice(agent: &str, why: &str) -> String {
    format!(
        "{NOTICE_START}{why}. This agent no longer wakes for messages; restart it with \
         `pm agent restart {agent}`."
    )
}

/// How a wait ended.
#[derive(Debug, PartialEq)]
pub(super) enum Waited {
    /// Deliver this prompt to the agent.
    Wake(String),
    /// Nothing to deliver: superseded, the harness gone, or the loop stopped.
    Ended,
}

/// Run the waiter for `harness`. Returns the exit code: 2 with the
/// continuation on stderr for a rewake, else 0, or 1 once it failed, the
/// agent then marked unarmed.
pub(super) fn run(harness: Harness, on_turn: &mut dyn FnMut(AgentState, u32)) -> i32 {
    if std::env::var_os("PM_AGENT_NAME").is_none() {
        return 0;
    }
    run_on(harness, &read_stdin(), on_turn)
}

/// [`run`] on the hook payload `payload`, already read.
pub(super) fn run_on(
    harness: Harness,
    payload: &str,
    on_turn: &mut dyn FnMut(AgentState, u32),
) -> i32 {
    let caller = Caller::current();
    let Ok(agent) = std::env::var("PM_AGENT_NAME") else {
        return 0;
    };
    let Ok((project_root, scope)) = paths::agent_scope() else {
        return 0;
    };
    let busy = parse_busy(payload);
    runtime::log_stop_hook(
        &project_root,
        &scope,
        &agent,
        &format!("{harness} waiter: {}", background_work(payload)),
    );
    let waited = wait(
        harness,
        busy,
        &project_root,
        &scope,
        &agent,
        on_turn,
        |interval| {
            std::thread::sleep(interval);
            !caller.alive()
        },
    );
    let failed = |why: String, on_turn: &mut dyn FnMut(AgentState, u32)| {
        runtime::log_stop_hook(&project_root, &scope, &agent, &why);
        hook_ended(&project_root, &scope, &agent, why.clone(), on_turn);
        eprintln!("pm: Stop hook {why}; this agent is unarmed");
        1
    };
    let prompt = match waited {
        Ok(Waited::Wake(prompt)) => prompt,
        Ok(Waited::Ended) => return 0,
        Err(e) => return failed(format!("failed: {e}"), on_turn),
    };
    match harness.wake() {
        Wake::Rewake => {
            eprint!("{prompt}");
            2
        }
        Wake::Queue => match session_id(payload)
            .ok_or_else(|| "the Stop payload names no session".to_string())
            .and_then(|id| {
                harness
                    .queue_prompt(&id, &prompt)
                    .map_err(|e| e.to_string())
            }) {
            Ok(()) => 0,
            Err(e) => failed(format!("could not queue the continuation: {e}"), on_turn),
        },
        Wake::Block => failed("ran as a waiter for a harness it blocks".into(), on_turn),
    }
}

fn session_id(payload: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("session_id")?
        .as_str()
        .map(str::to_string)
}

/// Wait for `agent`'s inbox as its newest waiter. `gone` sleeps up to the
/// poll interval and returns whether the harness that ran the waiter is
/// gone.
fn wait(
    harness: Harness,
    busy: bool,
    project_root: &Path,
    scope: &str,
    agent: &str,
    on_turn: &mut dyn FnMut(AgentState, u32),
    mut gone: impl FnMut(Duration) -> bool,
) -> Result<Waited> {
    let log = |line: &str| runtime::log_stop_hook(project_root, scope, agent, line);
    let waiter = std::process::id();
    let newest = || runtime::read_waiter(project_root, scope, agent) == Some(waiter);
    let started = chrono::Utc::now();
    hooks_dialog::close_with_turn(project_root, scope, agent)?;
    runtime::take_waiter(project_root, scope, agent, waiter, busy.then_some(started))?;
    if runtime::loop_tripped(project_root, scope, agent).is_some() {
        log("the loop is stopped: ends at once");
        return Ok(Waited::Ended);
    }
    runtime::touch_activity(project_root, scope, agent)?;
    runtime::clear_waiting(project_root, scope, agent)?;
    let immediate = !unread_senders(project_root, scope, agent)?.is_empty();
    let marker = if busy {
        WaitingKind::Background
    } else {
        WaitingKind::Idle
    };
    if !immediate {
        let waiting = hooks_waiting::between_turns(project_root, scope, agent, harness, started);
        log(&format!("nothing unread: waits, marked {:?}", waiting.kind));
        runtime::write_waiting(project_root, scope, agent, &waiting)?;
        on_turn(hooks_waiting::state_of(&waiting), 0);
        let mut ended = false;
        agent_wait::agent_wait_while(project_root, scope, agent, None, None, |d| {
            ended = gone(d) || !newest();
            !ended
        })?;
        if ended || !newest() {
            log(if newest() {
                "its harness is gone: ends"
            } else {
                "superseded: ends"
            });
            return Ok(Waited::Ended);
        }
    }
    let messages_dir = paths::messages_dir(project_root);
    let read = messages::read_count(&messages_dir, scope, agent);
    if let Some(why) = trips(project_root, scope, agent, immediate, read)? {
        log(&format!("breaker tripped: {why}"));
        runtime::trip_loop(project_root, scope, agent, &why)?;
        return Ok(Waited::Wake(notice(agent, &why)));
    }
    log(if immediate {
        "messages unread as it started: wakes the agent"
    } else {
        "a message arrived: wakes the agent"
    });
    // The turn the wake starts ends what the waiter marked: its own marker,
    // or a dialog's it kept, which then stands for an agent mid-turn.
    if !runtime::clear_waiting_if(project_root, scope, agent, marker)?
        && let Some(held) =
            runtime::read_waiting(project_root, scope, agent).filter(|w| w.between_turns)
    {
        let held = Waiting {
            between_turns: false,
            ..held
        };
        runtime::write_waiting(project_root, scope, agent, &held)?;
    }
    runtime::touch_activity(project_root, scope, agent)?;
    on_turn(
        AgentState::Busy,
        messages::unread_count(&messages_dir, scope, agent),
    );
    Ok(Waited::Wake(reason(&unread_senders(
        project_root,
        scope,
        agent,
    )?)))
}

/// Count this wake on the breaker; `immediate` when it found messages
/// unread as it started, `read` how many the agent has read. Returns why
/// the loop stops, once it does.
fn trips(
    project_root: &Path,
    scope: &str,
    agent: &str,
    immediate: bool,
    read: u32,
) -> Result<Option<String>> {
    let wasted = match runtime::read_breaker(project_root, scope, agent) {
        Some(last) if immediate && last.read == read => last.wasted + 1,
        _ => 0,
    };
    runtime::write_breaker(project_root, scope, agent, &Breaker { wasted, read })?;
    Ok((wasted >= MAX_WASTED).then(|| {
        format!(
            "{wasted} consecutive wakes found messages unread and the agent had read none \
             since the last (`pm msg read` may not be reaching this agent's inbox)"
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_question_codex_drops_with_the_turn_leaves_the_agent_idle_not_asking() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let payload = serde_json::json!({"hook_event_name": "PreToolUse",
            "tool_name": "request_user_input_async", "tool_use_id": "c",
            "tool_input": {"questions": [{"title": "Which colour?"}]}});
        let (dialog, reply_context) = Harness::Codex.typed_dialog(&payload).unwrap();
        runtime::write_waiting(root, "login", "implementer", &dialog.waiting()).unwrap();
        let record = runtime::DialogRecord {
            dialog,
            pid: None,
            reply_context,
        };
        runtime::write_dialog(root, "login", "implementer", &record).unwrap();
        let mut turns = Vec::new();

        let waited = wait(
            Harness::Codex,
            false,
            root,
            "login",
            "implementer",
            &mut |state, _| turns.push(state),
            |_| true,
        )
        .unwrap();

        assert_eq!(waited, Waited::Ended);
        assert_eq!(turns, [AgentState::Idle]);
        assert!(runtime::dialog_closed(
            root,
            "login",
            "implementer",
            &record.dialog.id
        ));
    }
}
