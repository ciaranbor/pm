//! Whether a codex session's last turn failed, read from its rollout.
//!
//! An API error ends the turn with no hook — codex has no failure event
//! (0.160) — and leaves the agent at its composer. The rollout records the
//! turn's end as an `event_msg` whose payload is `task_complete`, carrying
//! an `error` when the turn failed. The session is failed while that is its
//! last turn boundary: a later `task_started` (or `turn_context`) means a
//! new turn began, and a `turn_aborted` is an interrupt, which codex
//! reports through its own hook.

use std::path::Path;

use serde_json::Value;

use crate::harness::transcript::{Cache, cached, entry_id, last_entry};
use crate::state::runtime::{Waiting, WaitingKind};

static CACHE: Cache<Waiting> = std::sync::Mutex::new(None);

/// The failure the session whose rollout is at `path` ended its last turn
/// with, if no turn has started since, dated by the rollout's mtime and
/// named by the turn's `turn_id` (else that mtime).
pub(in crate::harness) fn turn_ended(path: &Path) -> Option<Waiting> {
    cached(&CACHE, path, |len, mtime| {
        let end = last_entry(path, len, is_boundary)?;
        let payload = end
            .pointer("/payload")
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("task_complete"))?;
        let error = payload.get("error").filter(|e| !e.is_null())?;
        Some(Waiting {
            since: mtime.into(),
            entry: Some(entry_id(payload.get("turn_id"), mtime)),
            ..Waiting::now(WaitingKind::Error, Some(describe(error)))
        })
    })
}

/// Whether `entry` starts or ends a turn.
fn is_boundary(entry: &Value) -> bool {
    match entry.get("type").and_then(Value::as_str) {
        Some("turn_context") => true,
        Some("event_msg") => matches!(
            entry.pointer("/payload/type").and_then(Value::as_str),
            Some("task_started" | "task_complete" | "turn_aborted")
        ),
        _ => false,
    }
}

/// One line for `error`: the API's own message when `error.message` holds
/// the response body, else its first line.
fn describe(error: &Value) -> String {
    let raw = error.get("message").and_then(Value::as_str).unwrap_or("");
    let body = serde_json::from_str::<Value>(raw).ok();
    let inner = body
        .as_ref()
        .and_then(|b| b.pointer("/error/message"))
        .and_then(Value::as_str);
    let status = body
        .as_ref()
        .and_then(|b| b.get("status"))
        .and_then(Value::as_u64);
    let text = inner.unwrap_or(raw).lines().next().unwrap_or("").trim();
    match status {
        Some(status) => format!("API error {status}: {text}"),
        None if text.is_empty() => "API error".to_string(),
        None => format!("API error: {text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::transcript::testing::{append, bulky};
    use tempfile::tempdir;

    fn event(payload: Value) -> String {
        serde_json::json!({"timestamp": "2026-10-02T12:00:00.000Z", "type": "event_msg", "payload": payload})
            .to_string()
    }

    fn started() -> String {
        event(serde_json::json!({"type": "task_started", "turn_id": "t2"}))
    }

    fn completed() -> String {
        event(
            serde_json::json!({"type": "task_complete", "turn_id": "t1", "last_agent_message": "done"}),
        )
    }

    fn failed() -> String {
        event(serde_json::json!({
            "type": "task_complete",
            "turn_id": "t1",
            "last_agent_message": null,
            "error": {
                "message": "{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'no-such-model-xyz' model is not supported.\"}}",
                "codex_error_info": "other",
            },
        }))
    }

    fn token_count() -> String {
        event(serde_json::json!({"type": "token_count", "info": null}))
    }

    #[test]
    fn a_session_is_failed_until_its_next_turn_starts() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        append(&path, &[started(), completed()]);
        assert_eq!(turn_ended(&path), None);

        append(&path, &[started(), failed(), token_count(), bulky()]);
        let failure = turn_ended(&path).expect("failed");
        assert_eq!(failure.kind, WaitingKind::Error);
        assert_eq!(failure.entry.as_deref(), Some("t1"), "named by its turn");
        assert_eq!(
            failure.describe(),
            "API error 400: The 'no-such-model-xyz' model is not supported."
        );

        append(&path, &[started()]);
        assert_eq!(turn_ended(&path), None);
        append(&path, &[completed()]);
        assert_eq!(turn_ended(&path), None);
    }

    #[test]
    fn an_interrupted_turn_is_not_a_failure() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let aborted = event(serde_json::json!({"type": "turn_aborted", "reason": "interrupted"}));
        append(&path, &[failed(), started(), aborted]);
        assert_eq!(turn_ended(&path), None);
    }

    #[test]
    fn an_error_that_is_not_a_response_body_reads_as_its_first_line() {
        assert_eq!(
            describe(&serde_json::json!({"message": "stream disconnected\nretrying"})),
            "API error: stream disconnected"
        );
        assert_eq!(describe(&serde_json::json!({})), "API error");
    }
}
