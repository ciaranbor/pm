//! What codex's hooks say about an agent waiting on the user (verified on
//! 0.157).
//!
//! - An approval prompt (only under an approval policy other than pm's
//!   default `never`) fires `PermissionRequest`.
//! - A question (`request_user_input`, Plan mode only) fires only
//!   `PreToolUse`, carrying the questions.
//! - Either resolves with `PostToolUse`. Denying, or any interrupt of a
//!   turn, fires only `Interrupt` and skips Stop, leaving the agent at its
//!   prompt with no hook to wake it.
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
            Some(WaitingEvent::Set(Waiting::now(
                WaitingKind::Permission,
                detail,
            )))
        }
        "PreToolUse" if tool == Some(QUESTION_TOOL) => Some(WaitingEvent::Set(Waiting::now(
            WaitingKind::Question,
            payload
                .pointer("/tool_input/questions/0/question")
                .and_then(Value::as_str)
                .map(one_line),
        ))),
        "PostToolUse" => Some(WaitingEvent::Clear),
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
}
