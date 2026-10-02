//! Whether a Claude Code session was interrupted, read from its transcript.
//!
//! Esc mid-turn, or rejecting a tool dialog, fires no hook: the turn just
//! ends at the prompt. The transcript records it as a user entry whose text
//! is `[Request interrupted by user]` (`… for tool use]` for a dialog), and
//! the session is still interrupted while that is its last user or
//! assistant entry (verified on 2.1.284–2.1.287). Bookkeeping entries
//! (`system`, `attachment`, snapshots) may follow it, and subagent
//! entries are marked `isSidechain`.

use std::path::Path;

use serde_json::Value;

use crate::harness::transcript::{Cache, cached, entry_id, last_entry};
use crate::state::runtime::{Waiting, WaitingKind};

const INTERRUPTED: &str = "[Request interrupted by user";

static CACHE: Cache<Waiting> = std::sync::Mutex::new(None);

/// The interrupt the session whose transcript is at `path` is at, if
/// nothing has happened in it since, dated by the transcript's mtime and
/// named by the entry's `uuid` (else that mtime).
pub(in crate::harness) fn turn_ended(path: &Path) -> Option<Waiting> {
    cached(&CACHE, path, |len, mtime| {
        let interrupt = last_entry(path, len, is_turn_entry).filter(is_interrupt)?;
        Some(Waiting {
            since: mtime.into(),
            entry: Some(entry_id(interrupt.get("uuid"), mtime)),
            ..Waiting::now(WaitingKind::Interrupted, None)
        })
    })
}

/// A user or assistant entry of the main thread.
fn is_turn_entry(entry: &Value) -> bool {
    matches!(
        entry.get("type").and_then(Value::as_str),
        Some("user" | "assistant")
    ) && entry.get("isSidechain").and_then(Value::as_bool) != Some(true)
}

fn is_interrupt(entry: &Value) -> bool {
    if entry.get("type").and_then(Value::as_str) != Some("user") {
        return false;
    }
    match entry.pointer("/message/content") {
        Some(Value::String(text)) => text.starts_with(INTERRUPTED),
        Some(Value::Array(items)) => items.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("text")
                && item
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.starts_with(INTERRUPTED))
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::transcript::testing::{append, bulky};
    use tempfile::tempdir;

    fn user(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        })
        .to_string()
    }

    fn assistant() -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}]},
        })
        .to_string()
    }

    fn sidechain_assistant() -> String {
        serde_json::json!({"type": "assistant", "isSidechain": true, "message": {}}).to_string()
    }

    #[test]
    fn a_session_is_interrupted_until_its_next_turn() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        append(&path, &[user("do it"), assistant()]);
        assert_eq!(turn_ended(&path), None);

        append(
            &path,
            &[
                user("[Request interrupted by user]"),
                sidechain_assistant(),
                r#"{"type":"system","subtype":"turn_duration"}"#.to_string(),
                r#"{"type":"file-history-snapshot"}"#.to_string(),
            ],
        );
        let entry = turn_ended(&path).and_then(|w| w.entry);
        assert!(
            entry.is_some_and(|e| e.starts_with("mtime-")),
            "an entry with no uuid is named by the mtime"
        );

        append(&path, &[user("You have new messages")]);
        assert_eq!(turn_ended(&path), None);

        append(&path, &[user("[Request interrupted by user for tool use]")]);
        assert!(turn_ended(&path).is_some());
        append(&path, &[assistant()]);
        assert_eq!(turn_ended(&path), None);
    }

    #[test]
    fn an_interrupt_behind_a_long_entry_is_still_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        append(
            &path,
            &[assistant(), user("[Request interrupted by user]"), bulky()],
        );
        assert!(turn_ended(&path).is_some());
        assert_eq!(turn_ended(&dir.path().join("missing.jsonl")), None);
    }
}
