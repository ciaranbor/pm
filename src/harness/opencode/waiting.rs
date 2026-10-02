//! What the pm-never-idle plugin reports about an agent waiting on the
//! user. opencode has no hooks for it; the plugin watches the event stream
//! for permission asks and question forms of the session it drives,
//! tracks which are open, and sends a payload of pm's own shape: a
//! `PermissionRequest` or `Question` with a `detail` when one opens,
//! `Resolved` once none is left.

use serde_json::Value;

use crate::harness::{WaitingEvent, one_line};
use crate::state::runtime::{Waiting, WaitingKind};

pub(in crate::harness) fn event(payload: &Value) -> Option<WaitingEvent> {
    let detail = payload.get("detail").and_then(Value::as_str).map(one_line);
    let kind = match payload.get("hook_event_name")?.as_str()? {
        "PermissionRequest" => WaitingKind::Permission,
        "Question" => WaitingKind::Question,
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
            event(&json!({"hook_event_name": "Resolved"})),
            Some(WaitingEvent::Clear)
        );
        assert_eq!(event(&json!({"hook_event_name": "Other"})), None);
    }
}
