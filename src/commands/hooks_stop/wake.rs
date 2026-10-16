//! The waiter: pm's Stop hook on a harness that runs it in the background
//! once the turn has ended ([`Wake::Rewake`], [`Wake::Post`]), so the
//! agent sits at its prompt between turns and takes typing and keys as
//! the user's. A SessionStart hook that
//! [waits](crate::harness::Harness::waits_at_session_start) runs it too, so
//! the agent waits from its start without a first turn. On Claude Code,
//! UserPromptSubmit runs a turn waiter too ([`run_in_turn`]), so a turn the
//! user interrupts, which runs no Stop hook, still leaves one.
//!
//! It takes over the agent's waiter file and marks the agent idle (or
//! background, while background work runs, which the waiter file records
//! too: a prompt clears the marker, and an interrupt can leave this waiter
//! the agent's only one; or asking, while a subagent's dialog is still
//! open, or one its screen shows, [`super::screen`]), then waits on the
//! inbox. With messages unread it hands the continuation to the harness:
//! at once when some are unread as it starts. The harness never ends an earlier waiter
//! as a later turn starts, so one can outlive several turns and fire
//! mid-turn; each new one supersedes it, and a superseded waiter, or one
//! whose harness is gone, ends without a word or a marker. Whatever is
//! delivered twice — a mid-turn wake, a re-arm —
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
use std::time::{Duration, Instant};

use crate::commands::agent_wait;
use crate::commands::attention::AgentState;
use crate::commands::hook_process::Caller;
use crate::commands::msg_scheduled::Sweeper;
use crate::commands::running_agents::AgentAt;
use crate::commands::{agent_restart, hooks_dialog, hooks_user_prompt, hooks_waiting};
use crate::error::Result;
use crate::harness::{Harness, Wake};
use crate::messages;
use crate::state::paths;
use crate::state::runtime::{self, Breaker, Waiting, WaitingKind};
use crate::tmux;

use super::screen::Watch;
use super::{background_work, hook_ended, parse_busy, read_stdin, sweep};

/// Wasted wakes in a row that stop the loop.
pub(super) const MAX_WASTED: u32 = 5;

const NOTICE_START: &str = "pm: never-idle loop stopped: ";

/// Whether `prompt` is the notice of a loop that stopped itself.
pub(crate) fn is_loop_notice(prompt: &str) -> bool {
    prompt.trim_start().starts_with(NOTICE_START)
}

fn notice(project_root: &Path, scope: &str, agent: &str, why: &str) -> String {
    format!(
        "{NOTICE_START}{why}. This agent no longer wakes for messages; restart it with {}.",
        agent_restart::restart_command(&paths::project_name(project_root), scope, agent)
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
pub(super) fn run(
    harness: Harness,
    sweeper: Sweeper<'_>,
    on_turn: &mut dyn FnMut(AgentState, u32),
) -> i32 {
    if std::env::var_os("PM_AGENT_NAME").is_none() {
        return 0;
    }
    run_on(harness, &read_stdin(), sweeper, on_turn)
}

/// [`run`] on the hook payload `payload`, already read.
pub(super) fn run_on(
    harness: Harness,
    payload: &str,
    sweeper: Sweeper<'_>,
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
    let at = AgentAt {
        harness,
        project_root: &project_root,
        scope: &scope,
        name: &agent,
    };
    let pane = tmux::own_pane(sweeper.tmux_server);
    let capture = || {
        let pane = pane.as_deref()?;
        tmux::capture_screen(sweeper.tmux_server, pane).ok()
    };
    let waited = wait(at, busy, sweeper, on_turn, capture, |interval| {
        std::thread::sleep(interval);
        !caller.alive()
    });
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
        Wake::Post => match runtime::read_inbound(&project_root, &scope, &agent)
            .ok_or_else(|| "its session reported no inbound channel".to_string())
            .and_then(|inbound| {
                harness
                    .post_prompt(&inbound, &prompt)
                    .map_err(|e| e.to_string())
            }) {
            Ok(()) => 0,
            Err(e) => failed(format!("could not post the continuation: {e}"), on_turn),
        },
        Wake::Block => failed("ran as a waiter for a harness it blocks".into(), on_turn),
    }
}

/// Run `harness`'s waiter for the turn a prompt starts, from its
/// UserPromptSubmit hook, whose `payload` it has read. It wakes the agent
/// only for a message that arrives during the turn, which the harness
/// delivers after the turn's next step, or at once should the user
/// interrupt the turn: no Stop hook then starts a waiter. It leaves the
/// agent's marker alone, since the agent is busy, and ends without a word
/// for a prompt UserPromptSubmit drops, which starts no turn. Returns the
/// exit code, as [`run`] does.
pub(super) fn run_in_turn(
    harness: Harness,
    payload: &str,
    sweeper: Sweeper<'_>,
    on_turn: &mut dyn FnMut(AgentState, u32),
) -> i32 {
    let caller = Caller::current();
    let Ok(agent) = std::env::var("PM_AGENT_NAME") else {
        return 0;
    };
    let Ok((project_root, scope)) = paths::agent_scope() else {
        return 0;
    };
    let prompt = serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("prompt")?.as_str().map(str::to_string))
        .unwrap_or_default();
    let at = AgentAt {
        harness,
        project_root: &project_root,
        scope: &scope,
        name: &agent,
    };
    let waited = wait_in_turn(at, &prompt, sweeper, on_turn, |interval| {
        std::thread::sleep(interval);
        !caller.alive()
    });
    match waited {
        Ok(Waited::Wake(prompt)) if harness.wake() == Wake::Rewake => {
            eprint!("{prompt}");
            2
        }
        Ok(_) => 0,
        Err(e) => {
            let why = format!("turn waiter failed: {e}");
            runtime::log_stop_hook(&project_root, &scope, &agent, &why);
            1
        }
    }
}

/// Wait, as the agent's newest waiter, for a message to arrive in its inbox
/// after the wait began, unless `prompt` is one UserPromptSubmit drops.
/// `gone` is as for [`wait`].
fn wait_in_turn(
    at: AgentAt<'_>,
    prompt: &str,
    sweeper: Sweeper<'_>,
    on_turn: &mut dyn FnMut(AgentState, u32),
    mut gone: impl FnMut(Duration) -> bool,
) -> Result<Waited> {
    let AgentAt {
        harness,
        project_root,
        scope,
        name: agent,
    } = at;
    if hooks_user_prompt::is_duplicate(Some(harness), project_root, scope, agent, prompt) {
        return Ok(Waited::Ended);
    }
    let log = |line: &str| runtime::log_stop_hook(project_root, scope, agent, line);
    let waiter = std::process::id();
    let newest = || runtime::read_waiter(project_root, scope, agent) == Some(waiter);
    runtime::take_waiter(project_root, scope, agent, waiter, None)?;
    if runtime::loop_tripped(project_root, scope, agent).is_some() {
        log("turn waiter: the loop is stopped: ends at once");
        return Ok(Waited::Ended);
    }
    let messages_dir = paths::messages_dir(project_root);
    let sent = || {
        messages::read_count(&messages_dir, scope, agent)
            + messages::unread_count(&messages_dir, scope, agent)
    };
    let before = sent();
    log("turn waiter: waits for a message sent during the turn");
    loop {
        if gone(POLL) || !newest() {
            log(if newest() {
                "turn waiter: its harness is gone: ends"
            } else {
                "turn waiter: superseded: ends"
            });
            return Ok(Waited::Ended);
        }
        sweep(project_root, scope, agent, sweeper);
        if sent() > before && messages::unread_count(&messages_dir, scope, agent) > 0 {
            break;
        }
    }
    // A new message is no wasted wake, as after a Stop waiter's wait.
    let read = messages::read_count(&messages_dir, scope, agent);
    runtime::write_breaker(project_root, scope, agent, &Breaker { wasted: 0, read })?;
    log("turn waiter: a message arrived: wakes the agent");
    runtime::touch_activity(project_root, scope, agent)?;
    on_turn(
        AgentState::Busy,
        messages::unread_count(&messages_dir, scope, agent),
    );
    Ok(Waited::Wake(super::continuation(
        project_root,
        scope,
        agent,
    )?))
}

/// How often a turn waiter checks the inbox, as [`agent_wait`] does.
const POLL: Duration = Duration::from_secs(2);

/// Wait for `agent`'s inbox as its newest waiter, watching its screen
/// ([`Watch`]), which `capture` returns. `gone` sleeps up to the poll
/// interval and returns whether the harness that ran the waiter is gone.
fn wait(
    at: AgentAt<'_>,
    busy: bool,
    sweeper: Sweeper<'_>,
    on_turn: &mut dyn FnMut(AgentState, u32),
    capture: impl FnMut() -> Option<String>,
    mut gone: impl FnMut(Duration) -> bool,
) -> Result<Waited> {
    let AgentAt {
        harness,
        project_root,
        scope,
        name: agent,
    } = at;
    let log = |line: &str| runtime::log_stop_hook(project_root, scope, agent, line);
    let waiter = std::process::id();
    let newest = || runtime::read_waiter(project_root, scope, agent) == Some(waiter);
    let started = chrono::Utc::now();
    hooks_dialog::close_with_turn(project_root, scope, agent)?;
    runtime::take_waiter(project_root, scope, agent, waiter, busy.then_some(started))?;
    let woken_through = runtime::take_woken_through(project_root, scope, agent);
    if runtime::loop_tripped(project_root, scope, agent).is_some() {
        log("the loop is stopped: ends at once");
        return Ok(Waited::Ended);
    }
    runtime::touch_activity(project_root, scope, agent)?;
    runtime::clear_waiting(project_root, scope, agent)?;
    let immediate =
        messages::resolve_sender(&paths::messages_dir(project_root), scope, agent, None)?.is_some();
    // After `immediate`: a send this delivers arrives during the wait, so a
    // wake for it is never counted wasted.
    sweep(project_root, scope, agent, sweeper);
    let marker = if busy {
        WaitingKind::Background
    } else {
        WaitingKind::Idle
    };
    let mut marked = None;
    if !immediate {
        let waiting = hooks_waiting::between_turns(project_root, scope, agent, harness, started);
        log(&format!("nothing unread: waits, marked {:?}", waiting.kind));
        runtime::write_waiting(project_root, scope, agent, &waiting)?;
        on_turn(hooks_waiting::state_of(&waiting), 0);
        let until = harness
            .unhooked_dialog_window()
            .map(|window| Instant::now() + window);
        let mut watch = Watch::new(at, woken_through, started, until, capture);
        let mut watch_screen = |on_turn: &mut dyn FnMut(AgentState, u32)| match watch.check() {
            Ok(Some(state)) => {
                log(&format!("its screen says it is {state:?}"));
                on_turn(state, 0);
            }
            Ok(None) => {}
            Err(e) => log(&format!("could not mark what its screen shows: {e}")),
        };
        watch_screen(on_turn);
        let mut ended = false;
        agent_wait::agent_wait_while(project_root, scope, agent, None, None, |d| {
            ended = gone(d) || !newest();
            if !ended {
                sweep(project_root, scope, agent, sweeper);
                watch_screen(on_turn);
            }
            !ended
        })?;
        marked = watch.marked().cloned();
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
        return Ok(Waited::Wake(notice(project_root, scope, agent, &why)));
    }
    log(if immediate {
        "messages unread as it started: wakes the agent"
    } else {
        "a message arrived: wakes the agent"
    });
    // The turn the wake starts ends what the waiter marked: its own marker,
    // or a dialog's it kept, which then stands for an agent mid-turn. A
    // dialog's detail its screen doesn't show is kept for the next waiter.
    if !runtime::clear_waiting_if(project_root, scope, agent, marker)?
        && let Some(held) =
            runtime::read_waiting(project_root, scope, agent).filter(|w| w.between_turns)
    {
        if held.kind == WaitingKind::Dialog
            && held.subagent.is_none()
            && marked.as_ref() != Some(&held)
            && let Some(detail) = &held.detail
        {
            runtime::write_woken_through(project_root, scope, agent, detail)?;
        }
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
    Ok(Waited::Wake(super::continuation(
        project_root,
        scope,
        agent,
    )?))
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
    use crate::commands::hooks_stop::tests::{NO_SWEEP, sweeper};
    use tempfile::tempdir;

    #[test]
    fn a_turn_waiter_wakes_for_a_message_sent_during_the_turn_only() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let messages = paths::messages_dir(&root);
        messages::send(&messages, "login", "implementer", "reviewer", "before").unwrap();
        let mut polls = 0;
        let mut turns = Vec::new();

        let waited = wait_in_turn(
            at(&root),
            "go on",
            NO_SWEEP,
            &mut |state, unread| turns.push((state, unread)),
            |_| {
                polls += 1;
                if polls == 3 {
                    messages::send(&messages, "login", "implementer", "qa", "during").unwrap();
                }
                false
            },
        )
        .unwrap();

        assert_eq!(polls, 3, "the message unread as it began woke nothing");
        assert_eq!(
            waited,
            Waited::Wake(
                "[pm message] From reviewer@login: \"before\". Run `pm msg read` to read it. \
                 Also unread: qa@login. (pm delivers messages this way; this is not an error.)"
                    .into()
            )
        );
        assert_eq!(turns, [(AgentState::Busy, 2)]);
        assert_eq!(runtime::read_waiting(&root, "login", "implementer"), None);
    }

    #[test]
    fn a_turn_waiter_ends_once_its_turn_ends() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let waited = wait_in_turn(at(root), "go on", NO_SWEEP, &mut |_, _| {}, |_| {
            runtime::take_waiter(root, "login", "implementer", 1, None).unwrap();
            false
        })
        .unwrap();
        assert_eq!(waited, Waited::Ended);
    }

    fn claude_screen(name: &str) -> String {
        crate::testing::screen_fixture(&format!("claude-code-{name}"))
    }

    fn at(root: &Path) -> AgentAt<'_> {
        AgentAt {
            harness: Harness::ClaudeCode,
            project_root: root,
            scope: "login",
            name: "implementer",
        }
    }

    #[test]
    fn a_dropped_continuation_leaves_the_idle_waiter_in_charge() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let messages = paths::messages_dir(root);
        messages::send(&messages, "login", "implementer", "reviewer", "hi").unwrap();
        let continuation = super::super::continuation(root, "login", "implementer").unwrap();
        messages::next(&messages, "login", "implementer", "reviewer@login").unwrap();
        runtime::take_waiter(root, "login", "implementer", 1, None).unwrap();

        let waited = wait_in_turn(at(root), &continuation, NO_SWEEP, &mut |_, _| {}, |_| {
            panic!("waited on a prompt that starts no turn")
        })
        .unwrap();

        assert_eq!(waited, Waited::Ended);
        assert_eq!(runtime::read_waiter(root, "login", "implementer"), Some(1));
    }

    #[test]
    fn a_turn_waiter_of_a_stopped_loop_ends_at_once() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        runtime::trip_loop(root, "login", "implementer", "stopped").unwrap();
        let messages = paths::messages_dir(root);

        let waited = wait_in_turn(at(root), "go on", NO_SWEEP, &mut |_, _| {}, |_| {
            messages::send(&messages, "login", "implementer", "qa", "during").unwrap();
            false
        })
        .unwrap();

        assert_eq!(waited, Waited::Ended);
    }

    #[test]
    fn a_send_falling_due_as_a_waiter_starts_is_no_wasted_wake() {
        let server = crate::testing::TestServer::new();
        let dir = tempdir().unwrap();
        let (root, session, scope) =
            crate::commands::agent_send::tests::setup_project_with_tmux(dir.path(), &server);
        server.spawn_fake_agent(&root, &session, &scope, "implementer");
        let wasted = MAX_WASTED - 1;
        runtime::write_breaker(&root, "login", "implementer", &Breaker { wasted, read: 0 })
            .unwrap();
        super::super::tests::due_send(&root, "implementer");

        let waited = wait(
            at(&root),
            false,
            sweeper(&server),
            &mut |_, _| {},
            || None,
            |_| panic!("waited on a due send"),
        )
        .unwrap();

        assert_eq!(
            waited,
            Waited::Wake(
                "[pm message] From lead@login: \"reminder\". Run `pm msg read` to read it. (pm delivers messages this way; this is not an error.)".into()
            )
        );
        let breaker = runtime::read_breaker(&root, "login", "implementer").unwrap();
        assert_eq!(breaker.wasted, 0);
    }

    #[test]
    fn a_waiter_marks_the_agent_asking_while_its_screen_shows_a_dialog() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let closed = std::cell::Cell::new(false);
        let mut polls = 0;
        let mut turns = Vec::new();

        let waited = wait(
            at(root),
            false,
            NO_SWEEP,
            &mut |state, _| turns.push(state),
            || {
                Some(claude_screen(if closed.get() {
                    "draft"
                } else {
                    "teach-below"
                }))
            },
            |_| {
                polls += 1;
                closed.set(polls >= 2);
                polls == 4
            },
        )
        .unwrap();

        assert_eq!(waited, Waited::Ended);
        assert_eq!(
            turns,
            [AgentState::Idle, AgentState::Asking, AgentState::Idle]
        );
    }

    /// Runs a waiter that a message wakes once `open` has marked a dialog
    /// between turns; then the next waiter, its agent's screen showing
    /// `screen`. Returns the next waiter's marker.
    fn wake_through(root: &Path, open: impl Fn(), screen: &str) -> Waiting {
        let messages = paths::messages_dir(root);
        let mut polls = 0;
        let waited = wait(
            at(root),
            false,
            NO_SWEEP,
            &mut |_, _| {},
            || Some(claude_screen(screen)),
            |_| {
                polls += 1;
                if polls == 1 {
                    open();
                    messages::send(&messages, "login", "implementer", "qa", "hi").unwrap();
                }
                false
            },
        )
        .unwrap();
        assert!(matches!(waited, Waited::Wake(_)));
        messages::next(&messages, "login", "implementer", "qa@login").unwrap();
        runtime::clear_waiting(root, "login", "implementer").unwrap();

        let proposal = claude_screen("proposal");
        wait(
            at(root),
            false,
            NO_SWEEP,
            &mut |_, _| {},
            || Some(proposal.clone()),
            |_| true,
        )
        .unwrap();
        runtime::read_waiting(root, "login", "implementer").unwrap()
    }

    #[test]
    fn a_proposal_a_wake_ran_a_turn_through_still_asks_after_it() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let notified = Waiting {
            between_turns: true,
            ..Waiting::now(
                WaitingKind::Dialog,
                Some("Auto-mode setup proposal is ready for review".into()),
            )
        };
        let open = || runtime::write_waiting(root, "login", "implementer", &notified).unwrap();

        let marker = wake_through(root, open, "proposal");

        assert_eq!(marker.kind, WaitingKind::Dialog);
        assert_eq!(
            marker.describe(),
            "Auto-mode setup proposal is ready for review"
        );
    }

    #[test]
    fn a_title_read_off_the_screen_is_not_carried_to_another_dialog() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let marker = wake_through(root, || {}, "teach-below");

        assert_eq!(marker.kind, WaitingKind::Dialog);
        assert_eq!(marker.detail, None);
    }

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

        let codex = AgentAt {
            harness: Harness::Codex,
            ..at(root)
        };
        let waited = wait(
            codex,
            false,
            NO_SWEEP,
            &mut |state, _| turns.push(state),
            || None,
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
