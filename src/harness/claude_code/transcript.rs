//! Whether a Claude Code session was interrupted, read from its transcript.
//!
//! Esc mid-turn, or rejecting a tool dialog, fires no hook: the turn just
//! ends at the prompt. The transcript records it as a user entry whose text
//! is `[Request interrupted by user]` (`… for tool use]` for a dialog), and
//! the session is still interrupted while that is its last user or
//! assistant entry (verified on 2.1.284–2.1.287). Bookkeeping entries
//! (`system`, `attachment`, snapshots) may follow it, and subagent
//! entries are marked `isSidechain`.
//!
//! A dialog only a `Notification` reports ([`super::waiting`]) fires no hook
//! when it closes either. The auto-mode setup proposal records every way
//! out of it, saved, discarded or failed, as a main-thread `system` entry of
//! subtype `informational` (verified on 2.1.296); a dialog that resolves
//! into a turn, as "Session paused" does, records a user or assistant
//! entry. Either, timestamped after the marker, says the dialog is gone.
//! Other `system` entries (`away_summary`, `local_command`, `turn_duration`)
//! are written while one is still open. The notification fires only once
//! the dialog has been shown 6s without input, so it always precedes the
//! entry that closes the dialog.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::harness::transcript::{Cache, cached, entry_id, last_entry};
use crate::state::runtime::{Waiting, WaitingKind};

const INTERRUPTED: &str = "[Request interrupted by user";

static CACHE: Cache<Waiting> = std::sync::Mutex::new(None);
static CLOSES: Cache<DateTime<Utc>> = std::sync::Mutex::new(None);

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

/// Whether the session whose transcript is at `path` recorded an entry
/// that closed a notification-reported dialog open since `since`.
pub(in crate::harness) fn dialog_closed(path: &Path, since: DateTime<Utc>) -> bool {
    cached(&CLOSES, path, |len, _| {
        let entry = last_entry(path, len, closes_dialog)?;
        entry
            .get("timestamp")?
            .as_str()?
            .parse::<DateTime<Utc>>()
            .ok()
    })
    .is_some_and(|closed| closed > since)
}

fn closes_dialog(entry: &Value) -> bool {
    is_turn_entry(entry)
        || (entry.get("type").and_then(Value::as_str) == Some("system")
            && entry.get("subtype").and_then(Value::as_str) == Some("informational")
            && entry.get("isSidechain").and_then(Value::as_bool) != Some(true))
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
    fn a_notified_dialog_closes_at_a_later_informational_or_turn_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let at = |secs: i64| {
            DateTime::parse_from_rfc3339("2026-10-12T22:00:00Z")
                .unwrap()
                .to_utc()
                + chrono::Duration::seconds(secs)
        };
        let entry = |json: Value, secs: i64| {
            let mut json = json;
            json["timestamp"] = at(secs).to_rfc3339().into();
            json.to_string()
        };
        let system = |subtype: &str| serde_json::json!({"type": "system", "subtype": subtype});
        let since = at(10);

        append(
            &path,
            &[
                entry(serde_json::json!({"type": "assistant", "message": {}}), 0),
                entry(system("informational"), 5),
            ],
        );
        assert!(!dialog_closed(&path, since), "entries before it opened");

        append(
            &path,
            &[
                entry(system("away_summary"), 20),
                entry(system("local_command"), 21),
                entry(system("turn_duration"), 22),
                entry(serde_json::json!({"type": "file-history-snapshot"}), 23),
                entry(
                    serde_json::json!({"type": "assistant", "isSidechain": true, "message": {}}),
                    24,
                ),
                entry(
                    serde_json::json!({"type": "system", "subtype": "informational",
                                       "isSidechain": true}),
                    25,
                ),
            ],
        );
        assert!(!dialog_closed(&path, since), "still open");

        append(&path, &[entry(system("informational"), 30)]);
        assert!(dialog_closed(&path, since));

        append(
            &path,
            &[entry(
                serde_json::json!({"type": "user", "message": {}}),
                40,
            )],
        );
        assert!(dialog_closed(&path, at(35)), "a turn");
        assert!(!dialog_closed(&path, at(45)), "a later dialog");
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
