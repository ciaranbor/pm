//! What the pm-never-idle plugin reports about an agent waiting on the
//! user. opencode has no hooks for it; the plugin watches the event stream
//! of the session it drives, and sends a payload of pm's own shape with a
//! `detail`: `TurnFailed` when a turn fails, and for the dialogs, which it
//! tracks while open, `PermissionRequest`, `Question` (the question tool's
//! form) or `Dialog` (any other form) when one opens, `Resolved` once none
//! is left.
//!
//! In opencode 2.0.18 a permission ask and a form are the only waits on the
//! user with an event: every form the server raises (the question tool, the
//! web search provider choice, an MCP server's elicitation) is one, and the
//! TUI's own dialogs (provider login, model and agent pickers) are opened by
//! the user, not the agent. An MCP server that needs sign-in only changes
//! its status (`mcp.status.changed`, `needs_auth`); no turn waits on it. A
//! turn opencode retries after an API error is still the agent at work.

use serde_json::Value;

use crate::harness::{WaitingEvent, one_line};
use crate::state::runtime::{Waiting, WaitingKind};

pub(in crate::harness) fn event(payload: &Value) -> Option<WaitingEvent> {
    let detail = payload.get("detail").and_then(Value::as_str).map(one_line);
    let kind = match payload.get("hook_event_name")?.as_str()? {
        "PermissionRequest" => WaitingKind::Permission,
        "Question" => WaitingKind::Question,
        "Dialog" => WaitingKind::Dialog,
        "TurnFailed" => WaitingKind::Error,
        "Resolved" => return Some(WaitingEvent::Clear),
        _ => return None,
    };
    Some(WaitingEvent::Set(Waiting::now(kind, detail)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_plugins_payloads_set_and_clear() {
        let kind = |payload: Value| match event(&payload) {
            Some(WaitingEvent::Set(w)) => Some((w.kind, w.detail)),
            _ => None,
        };
        assert_eq!(
            kind(json!({"hook_event_name": "PermissionRequest", "detail": "edit src/*"})),
            Some((WaitingKind::Permission, Some("edit src/*".into())))
        );
        assert_eq!(
            kind(json!({"hook_event_name": "Question", "detail": "Which DB?"})),
            Some((WaitingKind::Question, Some("Which DB?".into())))
        );
        assert_eq!(
            kind(json!({"hook_event_name": "Dialog", "detail": "docs is requesting input"})),
            Some((WaitingKind::Dialog, Some("docs is requesting input".into())))
        );
        assert_eq!(
            kind(json!({"hook_event_name": "TurnFailed", "detail": "Model unavailable"})),
            Some((WaitingKind::Error, Some("Model unavailable".into())))
        );
        assert_eq!(
            event(&json!({"hook_event_name": "Resolved"})),
            Some(WaitingEvent::Clear)
        );
        assert_eq!(event(&json!({"hook_event_name": "Other"})), None);
    }
}
