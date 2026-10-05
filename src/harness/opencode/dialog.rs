//! opencode's permission asks as remote-answerable [`Dialog`]s (verified on
//! opencode 2.0.23; plugin docs: opencode.ai/v2/docs/build/plugins). The
//! pm-never-idle plugin hands `pm harness hooks dialog opencode` the
//! `permission.asked` event's data, `{id, sessionID, action, resources[],
//! …}`, and replies in-process with `ctx.permission.reply` using the
//! decision printed, `{decision: once|always|reject, message?}`.
//!
//! - Replying to a request the TUI already settled throws "Permission
//!   request not found", which the plugin takes as answered locally.
//! - `reject` also rejects the session's other pending requests; its
//!   message reaches the model.
//! - Pending requests never time out.
//! - pm launches opencode with `--auto` unless `[harness.opencode] auto =
//!   false`, and the TUI then replies `once` to every ask itself.
//!
//! The question tool's form has no reply API a plugin can reach, so it
//! stays a reported question, answered at the terminal.

use serde_json::{Value, json};

use crate::state::runtime::{Answer, Choice, Dialog, WaitingKind};

const ONCE: &str = "once";
const ALWAYS: &str = "always";
const REJECT: &str = "reject";

/// The dialog a `permission.asked` event's data opens.
pub(in crate::harness) fn dialog(payload: &Value) -> Option<(Dialog, Value)> {
    payload.get("id")?.as_str()?;
    let action = payload.get("action").and_then(Value::as_str).unwrap_or("");
    let resources: Vec<&str> = payload
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let dialog = Dialog {
        tool: Some(action.to_string()).filter(|a| !a.is_empty()),
        detail: Some(resources.join("\n")).filter(|r| !r.is_empty()),
        choices: vec![
            Choice::new(ONCE, "Allow once", false),
            Choice::new(ALWAYS, "Allow always", false),
            Choice::new(REJECT, "Reject", true),
        ],
        ..Dialog::new(WaitingKind::Permission)
    };
    Some((dialog, json!({})))
}

/// What the plugin passes to `ctx.permission.reply` for `answer`.
pub(in crate::harness) fn decision(answer: &Answer) -> Value {
    let mut out = json!({"decision": answer.choice});
    if let Some(message) = answer.message.as_ref().filter(|_| answer.choice == REJECT) {
        out["message"] = json!(message);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permission_ask_offers_opencodes_three_replies() {
        let (d, _) = dialog(&json!({
            "id": "per_1", "sessionID": "ses_1", "action": "edit",
            "resources": ["src/a.rs", "src/b.rs"]
        }))
        .unwrap();
        assert_eq!(d.kind, WaitingKind::Permission);
        assert_eq!(d.tool.as_deref(), Some("edit"));
        assert_eq!(d.detail.as_deref(), Some("src/a.rs\nsrc/b.rs"));
        let ids: Vec<&str> = d.choices.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, [ONCE, ALWAYS, REJECT]);

        let answer = |choice: &str, message: Option<&str>| Answer {
            id: d.id.clone(),
            choice: choice.into(),
            answers: Default::default(),
            message: message.map(str::to_string),
        };
        assert_eq!(decision(&answer(ONCE, None)), json!({"decision": "once"}));
        assert_eq!(
            decision(&answer(REJECT, Some("not that file"))),
            json!({"decision": "reject", "message": "not that file"})
        );
        assert!(
            dialog(&json!({"action": "edit"})).is_none(),
            "no request id"
        );
    }
}
