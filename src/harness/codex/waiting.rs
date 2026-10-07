//! What codex's hooks say about an agent waiting on the user (verified on
//! 0.157; approvals, denial and subagents again on 0.160).
//!
//! - An approval prompt (only under an approval policy other than pm's
//!   default `never`) fires `PermissionRequest`.
//! - A question (`request_user_input`, Plan mode only, verified on 0.160)
//!   fires only `PreToolUse`, carrying the questions.
//! - Either resolves with `PostToolUse`. Outside Plan mode 0.160 offers
//!   `request_user_input_async`, which returns at once while the question
//!   stays pending in the TUI: a dialog read from the hook payload, not
//!   here ([`super::dialog`]). Denying, or any interrupt of a
//!   turn, fires only `Interrupt` and skips Stop, leaving the agent at its
//!   prompt with no hook to wake it.
//! - Hooks fired inside a subagent (`spawn_agent`) carry its `agent_id`, so
//!   its `PostToolUse` resolves only a dialog of its own.
//!
//! Codex has no idle or notification event.

use serde_json::Value;

use crate::harness::{WaitingEvent, one_line};
use crate::state::runtime::{Waiting, WaitingKind};

pub(in crate::harness) const EVENTS: &[&str] = &[
    "PermissionRequest",
    "PreToolUse",
    "PostToolUse",
    "Interrupt",
];

const QUESTION_TOOL: &str = "request_user_input";

pub(in crate::harness) fn event(payload: &Value) -> Option<WaitingEvent> {
    let tool = payload.get("tool_name").and_then(Value::as_str);
    let subagent = payload
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    match payload.get("hook_event_name")?.as_str()? {
        "PermissionRequest" => {
            let command = payload
                .pointer("/tool_input/command")
                .and_then(Value::as_str)
                .map(one_line);
            let detail = match (tool, command) {
                (Some(tool), Some(command)) => Some(format!("{tool}: {command}")),
                (tool, command) => command.or(tool.map(str::to_string)),
            };
            Some(WaitingEvent::Set(Waiting {
                subagent,
                ..Waiting::now(WaitingKind::Permission, detail)
            }))
        }
        "PreToolUse" if tool == Some(QUESTION_TOOL) => Some(WaitingEvent::Set(Waiting {
            subagent,
            ..Waiting::now(
                WaitingKind::Question,
                payload
                    .pointer("/tool_input/questions/0/question")
                    .and_then(Value::as_str)
                    .map(one_line),
            )
        })),
        "PostToolUse" => Some(match subagent {
            Some(subagent) => WaitingEvent::ClearSubagent(subagent),
            None => WaitingEvent::Clear,
        }),
        "Interrupt" => Some(WaitingEvent::Set(Waiting::now(
            WaitingKind::Interrupted,
            None,
        ))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(payload: Value) -> (WaitingKind, Option<String>) {
        match event(&payload) {
            Some(WaitingEvent::Set(w)) => (w.kind, w.detail),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn approvals_and_questions_ask_and_an_interrupt_unarms() {
        assert_eq!(
            set(
                json!({"hook_event_name": "PermissionRequest", "turn_id": "t",
                       "permission_mode": "default", "tool_name": "Bash",
                       "tool_input": {"command": "cargo publish", "description": "x"}})
            ),
            (WaitingKind::Permission, Some("Bash: cargo publish".into()))
        );
        assert_eq!(
            set(
                json!({"hook_event_name": "PreToolUse", "tool_name": "request_user_input",
                       "tool_use_id": "c1",
                       "tool_input": {"questions": [{"id": "q", "header": "DB",
                                      "question": "Which DB?", "options": []}]}})
            ),
            (WaitingKind::Question, Some("Which DB?".into()))
        );
        assert_eq!(
            set(json!({"hook_event_name": "Interrupt", "turn_id": "t"})),
            (WaitingKind::Interrupted, None)
        );
    }

    #[test]
    fn only_a_question_tool_starting_and_any_tool_ending_count() {
        assert_eq!(
            event(
                &json!({"hook_event_name": "PreToolUse", "tool_name": "shell",
                          "tool_input": {"command": "ls"}})
            ),
            None
        );
        assert_eq!(
            event(&json!({"hook_event_name": "PostToolUse", "tool_name": "shell"})),
            Some(WaitingEvent::Clear)
        );
    }

    #[test]
    fn a_subagents_dialog_is_its_own() {
        // The fields codex 0.160 adds inside a `spawn_agent` subagent.
        let inside = |event: &str| {
            json!({"hook_event_name": event, "tool_name": "Bash",
                   "agent_id": "01a1", "agent_type": "default",
                   "tool_input": {"command": "ls"}})
        };
        match event(&inside("PermissionRequest")) {
            Some(WaitingEvent::Set(w)) => assert_eq!(w.subagent.as_deref(), Some("01a1")),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            event(&inside("PostToolUse")),
            Some(WaitingEvent::ClearSubagent("01a1".into()))
        );
    }
}
