//! What Claude Code's hooks say about an agent waiting on the user
//! (verified on 2.1.287).
//!
//! - `PermissionRequest` fires the moment any tool dialog opens,
//!   `AskUserQuestion` and `ExitPlanMode` included, and never for an
//!   auto-approved tool. It carries no `tool_use_id`, so one marker stands
//!   for every open dialog.
//! - `PostToolUse` fires when a dialog is approved or answered; rejecting
//!   one ends the turn with no event at all, so the marker stays until the
//!   user next types (UserPromptSubmit), but the transcript records the
//!   rejection ([`super::transcript`]). Hooks fired inside a subagent
//!   carry its `agent_id`: its `PostToolUse` resolves only a dialog of its
//!   own, never the main thread's or another subagent's.
//! - `Notification` follows 6s after any dialog opens, tool or not (plan
//!   approval, MCP elicitation), and `idle_prompt` once the agent has sat at
//!   its prompt for 60s with no Stop hook running.
//! - `StopFailure` fires instead of Stop when an API error ends the turn.
//!
//! Its stdout is read as a decision by `PermissionRequest`, so the handler
//! prints nothing.

use serde_json::Value;

use crate::harness::{WaitingEvent, one_line};
use crate::state::runtime::{Waiting, WaitingClass, WaitingKind};

pub(in crate::harness) const EVENTS: &[&str] = &[
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "StopFailure",
    "Notification",
];

/// Notification types that stand for a dialog on screen.
const DIALOGS: &[&str] = &[
    "permission_prompt",
    "elicitation_dialog",
    "elicitation_url_dialog",
    "agent_needs_input",
];

pub(in crate::harness) fn event(payload: &Value) -> Option<WaitingEvent> {
    let text = |key: &str| payload.get(key).and_then(Value::as_str).map(one_line);
    match payload.get("hook_event_name")?.as_str()? {
        "PermissionRequest" => Some(WaitingEvent::Set(Waiting {
            subagent: text("agent_id"),
            ..permission(payload)
        })),
        "PostToolUse" | "PostToolUseFailure" => Some(match text("agent_id") {
            Some(subagent) => WaitingEvent::ClearSubagent(subagent),
            None => WaitingEvent::Clear,
        }),
        "StopFailure" => Some(WaitingEvent::Set(Waiting::now(
            WaitingKind::Error,
            text("error").map(|e| format!("API error: {e}")),
        ))),
        "Notification" => {
            let kind = payload.get("notification_type")?.as_str()?;
            if DIALOGS.contains(&kind) {
                Some(WaitingEvent::Fill {
                    waiting: Waiting::now(WaitingKind::Dialog, text("message")),
                    over: &[WaitingClass::Unarmed, WaitingClass::Background],
                })
            } else if kind == "idle_prompt" {
                Some(WaitingEvent::Fill {
                    waiting: Waiting::now(WaitingKind::Prompt, None),
                    over: &[WaitingClass::Asking],
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn permission(payload: &Value) -> Waiting {
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let input = payload.get("tool_input");
    match tool {
        "AskUserQuestion" => Waiting::now(
            WaitingKind::Question,
            input
                .and_then(|i| i.pointer("/questions/0/question"))
                .and_then(Value::as_str)
                .map(one_line),
        ),
        "ExitPlanMode" => Waiting::now(WaitingKind::Plan, None),
        _ => {
            let target = ["command", "file_path", "url", "path", "pattern"]
                .iter()
                .find_map(|key| input?.get(key)?.as_str());
            let detail = match target {
                Some(target) => format!("{tool}: {}", one_line(target)),
                None => tool.to_string(),
            };
            Waiting::now(WaitingKind::Permission, Some(detail))
        }
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
    fn a_permission_request_says_which_dialog_and_what_it_is_about() {
        let request = |tool: &str, input: Value| {
            json!({"hook_event_name": "PermissionRequest", "permission_mode": "default",
                   "tool_name": tool, "tool_input": input})
        };
        assert_eq!(
            set(request(
                "AskUserQuestion",
                json!({"questions": [{"question": "Which DB?\nPick one", "header": "DB",
                       "options": [], "multiSelect": false}]})
            )),
            (WaitingKind::Question, Some("Which DB?".into()))
        );
        assert_eq!(
            set(request("ExitPlanMode", json!({}))),
            (WaitingKind::Plan, None)
        );
        assert_eq!(
            set(request(
                "Bash",
                json!({"command": "rm -rf build", "description": "x"})
            )),
            (WaitingKind::Permission, Some("Bash: rm -rf build".into()))
        );
        assert_eq!(
            set(request("Edit", json!({"file_path": "/src/a.rs"}))),
            (WaitingKind::Permission, Some("Edit: /src/a.rs".into()))
        );
    }

    #[test]
    fn a_resolved_tool_clears_and_an_api_error_unarms() {
        for name in ["PostToolUse", "PostToolUseFailure"] {
            assert_eq!(
                event(&json!({"hook_event_name": name, "tool_use_id": "t1"})),
                Some(WaitingEvent::Clear)
            );
            assert_eq!(
                event(&json!({"hook_event_name": name, "tool_use_id": "t2",
                              "agent_id": "a1", "agent_type": "general-purpose"})),
                Some(WaitingEvent::ClearSubagent("a1".into())),
                "a subagent's tool"
            );
        }
        assert_eq!(
            set(
                json!({"hook_event_name": "StopFailure", "error": "rate_limit",
                       "error_details": "429"})
            ),
            (WaitingKind::Error, Some("API error: rate_limit".into()))
        );
    }

    #[test]
    fn notifications_fill_in_without_hiding_a_more_specific_marker() {
        let notification = |kind: &str| {
            event(
                &json!({"hook_event_name": "Notification", "notification_type": kind,
                          "message": "Claude Code needs your approval for the plan"}),
            )
        };
        match notification("permission_prompt") {
            Some(WaitingEvent::Fill { waiting, over }) => {
                assert_eq!(waiting.kind, WaitingKind::Dialog);
                assert_eq!(
                    waiting.detail.as_deref(),
                    Some("Claude Code needs your approval for the plan")
                );
                assert_eq!(over, [WaitingClass::Unarmed, WaitingClass::Background]);
            }
            other => panic!("{other:?}"),
        }
        match notification("idle_prompt") {
            Some(WaitingEvent::Fill { waiting, over }) => {
                assert_eq!(waiting.kind, WaitingKind::Prompt);
                assert_eq!(over, [WaitingClass::Asking]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(notification("auth_success"), None);
        assert_eq!(event(&json!({"hook_event_name": "PreToolUse"})), None);
    }
}
