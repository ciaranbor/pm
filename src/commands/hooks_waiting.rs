//! `pm harness hooks waiting <harness>`: keeps the agent's waiting marker
//! ([`runtime`]) in step with what its harness reports — a dialog opening
//! or resolving, an interrupt, a failed turn — so the attention view can
//! tell an agent waiting on the user from one at work. Which events say
//! what is the harness's own knowledge ([`Harness::waiting_event`]); the
//! harness is named on the command line because the hooks file it is
//! installed in, or the plugin calling it, already knows it.
//!
//! It runs on every tool call, so the common path — a tool finished, no
//! marker — costs one stamp of the agent's activity, one failed unlink and
//! one failed read of its dialogs dir, with no registry read. Its stdout can
//! approve or deny a tool, so it prints nothing and always exits 0.

use std::io::Read;
use std::path::Path;

use crate::commands::attention::AgentState;
use crate::commands::hooks_dialog;
use crate::error::Result;
use crate::harness::{Harness, WaitingEvent};
use crate::messages;
use crate::state::paths;
use crate::state::runtime::{self, Dialog, Waiting, WaitingKind};
use chrono::{DateTime, Utc};

/// Run the hook. `on_change` is told the agent's new state and unread count
/// when its marker changed.
pub fn waiting(harness: Harness, on_change: impl FnOnce(AgentState, u32)) -> i32 {
    if let Ok(Some((state, unread))) = waiting_inner(harness) {
        on_change(state, unread);
    }
    0
}

fn waiting_inner(harness: Harness) -> Result<Option<(AgentState, u32)>> {
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
    let (project_root, scope) = paths::agent_scope()?;
    runtime::touch_activity(&project_root, &scope, &agent)?;
    let Some(state) = settle(&project_root, &scope, &agent, harness, &payload)? else {
        return Ok(None);
    };
    let unread = messages::unread_count(&paths::messages_dir(&project_root), &scope, &agent);
    Ok(Some((state, unread)))
}

/// Apply what `payload` says to the agent's dialogs and marker. A marker
/// cleared while dialogs are still open is set to stand for the oldest, the
/// one a terminal queueing several shows. Between turns, a marker a
/// subagent's event sets stays between turns, and one it clears gives way
/// to the waiter's ([`between_turns`]). Returns the agent's state when the
/// marker changed.
fn settle(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    payload: &serde_json::Value,
) -> Result<Option<AgentState>> {
    hooks_dialog::resolve(project_root, scope, agent, harness, payload)?;
    let between =
        runtime::read_waiting(project_root, scope, agent).is_some_and(|w| w.between_turns);
    let mut event = harness.waiting_event(payload);
    let subagents = matches!(event, Some(WaitingEvent::ClearSubagent(_)));
    if let Some(WaitingEvent::Set(w) | WaitingEvent::Fill { waiting: w, .. }) = &mut event {
        w.between_turns = between && w.subagent.is_some();
    }
    let state = apply(project_root, scope, agent, event)?;
    if state != Some(AgentState::Busy) {
        return Ok(state);
    }
    after_clear(project_root, scope, agent, harness, between && subagents)
}

/// Re-mark an agent whose marker was just cleared: between turns, with the
/// waiter's marker; otherwise for the oldest dialog still open, if any.
fn after_clear(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    between: bool,
) -> Result<Option<AgentState>> {
    let open = hooks_dialog::open_dialogs(project_root, scope, agent, harness);
    let waiting = match open.first() {
        _ if between => between_turns(project_root, scope, agent, harness, Utc::now()),
        Some(open) => open.dialog.waiting(),
        None => return Ok(Some(AgentState::Busy)),
    };
    runtime::write_waiting(project_root, scope, agent, &waiting)?;
    Ok(Some(state_of(&waiting)))
}

/// Re-mark the agent once its `dialog` closed through its own hook, so its
/// marker stops standing for it at once rather than when the tool ends.
/// Returns the agent's state when the marker changed.
pub(crate) fn dialog_closed(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    dialog: &Dialog,
) -> Result<Option<AgentState>> {
    let Some(held) = runtime::read_waiting(project_root, scope, agent)
        .filter(|w| w.kind == dialog.kind && w.subagent == dialog.subagent)
    else {
        return Ok(None);
    };
    runtime::clear_waiting(project_root, scope, agent)?;
    after_clear(project_root, scope, agent, harness, held.between_turns)
}

/// The marker for an agent whose turn has ended: the oldest of its dialogs
/// still open (Claude Code runs subagents in the background, so theirs can
/// outlive the turn), else background while its waiter waits on background
/// work as well as the inbox, else idle since `idle_since`.
pub(crate) fn between_turns(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    idle_since: DateTime<Utc>,
) -> Waiting {
    let waiting = match hooks_dialog::open_dialogs(project_root, scope, agent, harness).first() {
        Some(open) => open.dialog.waiting(),
        None => match runtime::waiter_background(project_root, scope, agent) {
            Some(since) => Waiting {
                since,
                ..Waiting::now(WaitingKind::Background, None)
            },
            None => Waiting {
                since: idle_since,
                ..Waiting::now(WaitingKind::Idle, None)
            },
        },
    };
    Waiting {
        between_turns: true,
        ..waiting
    }
}

/// The state an agent at `waiting` is in.
pub(crate) fn state_of(waiting: &Waiting) -> AgentState {
    match waiting.kind {
        WaitingKind::Idle => AgentState::Idle,
        kind => AgentState::from(kind.class()),
    }
}

/// Apply `event` to the agent's marker. Returns the agent's state when the
/// marker changed.
pub(crate) fn apply(
    project_root: &Path,
    scope: &str,
    agent: &str,
    event: Option<WaitingEvent>,
) -> Result<Option<AgentState>> {
    let waiting = match event {
        None => return Ok(None),
        Some(WaitingEvent::Clear) => {
            let cleared = runtime::clear_waiting(project_root, scope, agent)?;
            return Ok(cleared.then_some(AgentState::Busy));
        }
        Some(WaitingEvent::ClearSubagent(subagent)) => {
            let held = runtime::read_waiting(project_root, scope, agent);
            if held.is_none_or(|h| h.subagent.as_deref() != Some(&subagent)) {
                return Ok(None);
            }
            runtime::clear_waiting(project_root, scope, agent)?;
            return Ok(Some(AgentState::Busy));
        }
        Some(WaitingEvent::Set(waiting)) => waiting,
        Some(WaitingEvent::Fill { waiting, over }) => {
            let held = runtime::read_waiting(project_root, scope, agent);
            if held.is_some_and(|h| !over.contains(&h.kind.class())) {
                return Ok(None);
            }
            waiting
        }
    };
    runtime::write_waiting(project_root, scope, agent, &waiting)?;
    Ok(Some(AgentState::from(waiting.kind.class())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::runtime::{Waiting, WaitingKind};
    use serde_json::json;
    use tempfile::tempdir;

    fn send(root: &Path, harness: Harness, payload: serde_json::Value) -> Option<AgentState> {
        settle(root, "login", "implementer", harness, &payload).unwrap()
    }

    fn kind(root: &Path) -> Option<WaitingKind> {
        runtime::read_waiting(root, "login", "implementer").map(|w| w.kind)
    }

    #[test]
    fn a_dialog_asks_until_its_tool_resolves() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let asked = send(
            root,
            Harness::ClaudeCode,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
                   "tool_input": {"questions": [{"question": "Which DB?"}]}}),
        );
        assert_eq!(asked, Some(AgentState::Asking));
        assert_eq!(
            runtime::read_waiting(root, "login", "implementer")
                .unwrap()
                .describe(),
            "Which DB?"
        );

        let resolved = send(
            root,
            Harness::ClaudeCode,
            json!({"hook_event_name": "PostToolUse", "tool_use_id": "t1"}),
        );
        assert_eq!(resolved, Some(AgentState::Busy));
        assert_eq!(kind(root), None);
    }

    #[test]
    fn a_subagents_dialog_is_resolved_only_by_that_subagents_tools() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let tool_done = |agent_id: Option<&str>| {
            let mut payload = json!({"hook_event_name": "PostToolUse", "tool_use_id": "t"});
            if let Some(id) = agent_id {
                payload["agent_id"] = json!(id);
            }
            send(root, Harness::ClaudeCode, payload)
        };
        let asked = send(
            root,
            Harness::ClaudeCode,
            json!({"hook_event_name": "PermissionRequest", "agent_id": "a1",
                   "agent_type": "general-purpose", "tool_name": "Bash",
                   "tool_input": {"command": "cargo test"}}),
        );
        assert_eq!(asked, Some(AgentState::Asking));

        assert_eq!(tool_done(Some("a2")), None, "another subagent's tool");
        assert_eq!(kind(root), Some(WaitingKind::Permission));
        assert_eq!(tool_done(Some("a1")), Some(AgentState::Busy));
        assert_eq!(kind(root), None);

        send(
            root,
            Harness::ClaudeCode,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash",
                   "tool_input": {"command": "ls"}}),
        );
        assert_eq!(tool_done(Some("a1")), None, "the main thread's dialog");
        assert_eq!(tool_done(None), Some(AgentState::Busy));
    }

    #[test]
    fn a_dialog_answered_at_the_terminal_leaves_the_marker_standing_for_the_oldest_still_open() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let start = chrono::Utc::now();
        let mut opened = 0;
        let mut ask = |subagent: &str, command: &str| {
            let payload = json!({"hook_event_name": "PermissionRequest", "agent_id": subagent,
                                 "tool_name": "Bash", "tool_input": {"command": command}});
            let (mut dialog, reply_context) = Harness::ClaudeCode.dialog(&payload).unwrap();
            opened += 1;
            dialog.since = start + chrono::Duration::seconds(opened);
            let record = runtime::DialogRecord {
                dialog,
                pid: std::process::id(),
                reply_context,
            };
            runtime::write_dialog(root, "login", "implementer", &record).unwrap();
            settle(root, "login", "implementer", Harness::ClaudeCode, &payload).unwrap()
        };
        assert_eq!(ask("a1", "touch a"), Some(AgentState::Asking));
        assert_eq!(ask("a2", "touch b"), Some(AgentState::Asking));
        assert_eq!(ask("a3", "touch c"), Some(AgentState::Asking));
        let done = |subagent: &str, command: &str| {
            let payload = json!({"hook_event_name": "PostToolUse", "agent_id": subagent,
                                 "tool_name": "Bash", "tool_input": {"command": command}});
            let state = settle(root, "login", "implementer", Harness::ClaudeCode, &payload);
            let marker = runtime::read_waiting(root, "login", "implementer");
            (state.unwrap(), marker.and_then(|m| m.subagent))
        };

        assert_eq!(
            done("a3", "touch c"),
            (Some(AgentState::Asking), Some("a1".into())),
            "the oldest, not the newest left"
        );
        assert_eq!(
            runtime::read_waiting(root, "login", "implementer")
                .unwrap()
                .describe(),
            "Bash: touch a"
        );
        assert_eq!(
            done("a1", "touch a"),
            (Some(AgentState::Asking), Some("a2".into()))
        );
        assert_eq!(done("a2", "touch b"), (Some(AgentState::Busy), None));
    }

    #[test]
    fn a_subagents_dialog_closing_between_turns_gives_way_to_the_waiters_marker() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let (scope, agent) = ("login", "implementer");
        let ask = json!({"hook_event_name": "PermissionRequest", "agent_id": "a1",
                         "tool_name": "Bash", "tool_input": {"command": "touch a"}});
        let done = json!({"hook_event_name": "PostToolUse", "agent_id": "a1",
                          "tool_name": "Bash", "tool_input": {"command": "touch a"}});
        let open_dialog = || {
            let (dialog, reply_context) = Harness::ClaudeCode.dialog(&ask).unwrap();
            let record = runtime::DialogRecord {
                dialog,
                pid: std::process::id(),
                reply_context,
            };
            runtime::write_dialog(root, scope, agent, &record).unwrap();
        };
        let waiter_since = Utc::now() - chrono::Duration::minutes(5);
        runtime::take_waiter(root, scope, agent, std::process::id(), Some(waiter_since)).unwrap();
        let stop = || {
            let waiting = between_turns(root, scope, agent, Harness::ClaudeCode, Utc::now());
            runtime::write_waiting(root, scope, agent, &waiting).unwrap();
            waiting.kind
        };
        let marker = || runtime::read_waiting(root, scope, agent).map(|w| (w.kind, w.since));

        // The subagent asks before the turn ends, and is answered after.
        open_dialog();
        settle(root, scope, agent, Harness::ClaudeCode, &ask).unwrap();
        assert_eq!(
            stop(),
            WaitingKind::Permission,
            "the Stop hook keeps it asking"
        );
        assert_eq!(
            settle(root, scope, agent, Harness::ClaudeCode, &done).unwrap(),
            Some(AgentState::Background)
        );
        assert_eq!(marker(), Some((WaitingKind::Background, waiter_since)));

        // It asks after the turn ended.
        open_dialog();
        settle(root, scope, agent, Harness::ClaudeCode, &ask).unwrap();
        assert_eq!(kind(root), Some(WaitingKind::Permission));
        assert_eq!(
            settle(root, scope, agent, Harness::ClaudeCode, &done).unwrap(),
            Some(AgentState::Background)
        );

        // Answered remotely: its hook closes it before the tool ends.
        open_dialog();
        settle(root, scope, agent, Harness::ClaudeCode, &ask).unwrap();
        let record = runtime::read_dialogs(root, scope, agent).pop().unwrap();
        runtime::close_dialog(root, scope, agent, &record.dialog.id, true).unwrap();
        assert_eq!(
            dialog_closed(root, scope, agent, Harness::ClaudeCode, &record.dialog).unwrap(),
            Some(AgentState::Background)
        );
        assert_eq!(
            settle(root, scope, agent, Harness::ClaudeCode, &done).unwrap(),
            None,
            "its PostToolUse changes nothing"
        );
        assert_eq!(kind(root), Some(WaitingKind::Background));

        // Mid-turn, a closed dialog leaves the agent busy.
        runtime::clear_waiting(root, scope, agent).unwrap();
        open_dialog();
        settle(root, scope, agent, Harness::ClaudeCode, &ask).unwrap();
        assert_eq!(
            settle(root, scope, agent, Harness::ClaudeCode, &done).unwrap(),
            Some(AgentState::Busy)
        );
        assert_eq!(kind(root), None);
    }

    #[test]
    fn a_finished_tool_with_no_marker_changes_and_writes_nothing() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        for (harness, payload) in [
            (
                Harness::ClaudeCode,
                json!({"hook_event_name": "PostToolUse"}),
            ),
            (
                Harness::Codex,
                json!({"hook_event_name": "PreToolUse", "tool_name": "shell"}),
            ),
            (Harness::Codex, json!({"hook_event_name": "PostToolUse"})),
        ] {
            assert_eq!(send(root, harness, payload), None);
        }
        assert!(!root.join(".pm").exists(), "nothing written");
    }

    #[test]
    fn a_dialog_notification_never_hides_the_question_it_follows() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let notify = |root: &Path, kind: &str| {
            send(
                root,
                Harness::ClaudeCode,
                json!({"hook_event_name": "Notification", "notification_type": kind,
                       "message": "Claude needs your permission"}),
            )
        };
        send(
            root,
            Harness::ClaudeCode,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
                   "tool_input": {"questions": [{"question": "Which DB?"}]}}),
        );
        assert_eq!(notify(root, "permission_prompt"), None);
        assert_eq!(kind(root), Some(WaitingKind::Question));

        // At its prompt, a question it was asked is no longer on screen.
        assert_eq!(notify(root, "idle_prompt"), Some(AgentState::Unarmed));
        assert_eq!(kind(root), Some(WaitingKind::Prompt));

        runtime::write_waiting(
            root,
            "login",
            "implementer",
            &Waiting::now(WaitingKind::Interrupted, None),
        )
        .unwrap();
        assert_eq!(notify(root, "idle_prompt"), None, "keeps the cause");
        assert_eq!(kind(root), Some(WaitingKind::Interrupted));

        assert_eq!(
            notify(root, "permission_prompt"),
            Some(AgentState::Asking),
            "a dialog after the interrupt"
        );
        assert_eq!(kind(root), Some(WaitingKind::Dialog));

        runtime::write_waiting(
            root,
            "login",
            "implementer",
            &Waiting::now(WaitingKind::Background, None),
        )
        .unwrap();
        assert_eq!(notify(root, "permission_prompt"), Some(AgentState::Asking));
        assert_eq!(kind(root), Some(WaitingKind::Dialog));
    }

    #[test]
    fn a_codex_interrupt_unarms_and_the_opencode_plugin_resolves() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        assert_eq!(
            send(
                root,
                Harness::Codex,
                json!({"hook_event_name": "Interrupt"})
            ),
            Some(AgentState::Unarmed)
        );
        assert_eq!(
            send(
                root,
                Harness::OpenCode,
                json!({"hook_event_name": "Question", "detail": "Which DB?"})
            ),
            Some(AgentState::Asking)
        );
        assert_eq!(
            send(
                root,
                Harness::OpenCode,
                json!({"hook_event_name": "Resolved"})
            ),
            Some(AgentState::Busy)
        );
        assert_eq!(kind(root), None);
    }
}
