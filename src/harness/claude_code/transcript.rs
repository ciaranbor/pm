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
//! Only the file's tail is read, and the answer is kept per file size and
//! mtime, so the long-running tmux watcher rereads a transcript only once
//! it has changed.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::Value;

const INTERRUPTED: &str = "[Request interrupted by user";

/// The tail read first; doubled while it holds no complete user or
/// assistant entry, up to [`MAX_TAIL`].
const TAIL: u64 = 64 * 1024;
const MAX_TAIL: u64 = 4 * 1024 * 1024;

type Seen = (u64, SystemTime, Option<SystemTime>);

static CACHE: Mutex<Option<HashMap<PathBuf, Seen>>> = Mutex::new(None);

/// When the session whose transcript is at `path` was interrupted, if it
/// has been and nothing has happened in it since: the transcript's mtime.
pub(in crate::harness) fn interrupted(path: &Path) -> Option<SystemTime> {
    let meta = std::fs::metadata(path).ok()?;
    let (len, mtime) = (meta.len(), meta.modified().ok()?);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(&(seen_len, seen_mtime, answer)) = cache.get(path)
        && (seen_len, seen_mtime) == (len, mtime)
    {
        return answer;
    }
    let answer = last_turn_entry(path, len)
        .filter(is_interrupt)
        .map(|_| mtime);
    cache.insert(path.to_path_buf(), (len, mtime, answer));
    answer
}

/// The last user or assistant entry of the main thread.
fn last_turn_entry(path: &Path, len: u64) -> Option<Value> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut tail = TAIL;
    loop {
        let start = len.saturating_sub(tail);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(len - start)
            .read_to_end(&mut bytes)
            .ok()?;
        let text = String::from_utf8_lossy(&bytes);
        // A tail that starts mid-file starts mid-line.
        let whole = match text.split_once('\n') {
            Some((_, rest)) if start > 0 => rest,
            None if start > 0 => "",
            _ => &text,
        };
        let found = whole
            .lines()
            .rev()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|entry| {
                matches!(
                    entry.get("type").and_then(Value::as_str),
                    Some("user" | "assistant")
                ) && entry.get("isSidechain").and_then(Value::as_bool) != Some(true)
            });
        if found.is_some() || start == 0 || tail >= MAX_TAIL {
            return found;
        }
        tail *= 2;
    }
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
    use std::io::Write;
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

    fn append(path: &Path, lines: &[String]) {
        let mut file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    #[test]
    fn a_session_is_interrupted_until_its_next_turn() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        append(&path, &[user("do it"), assistant()]);
        assert_eq!(interrupted(&path), None);

        append(
            &path,
            &[
                user("[Request interrupted by user]"),
                sidechain_assistant(),
                r#"{"type":"system","subtype":"turn_duration"}"#.to_string(),
                r#"{"type":"file-history-snapshot"}"#.to_string(),
            ],
        );
        assert!(interrupted(&path).is_some());

        append(&path, &[user("You have new messages")]);
        assert_eq!(interrupted(&path), None);

        append(&path, &[user("[Request interrupted by user for tool use]")]);
        assert!(interrupted(&path).is_some());
        append(&path, &[assistant()]);
        assert_eq!(interrupted(&path), None);
    }

    #[test]
    fn an_interrupt_behind_a_long_entry_is_still_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let bulky = serde_json::json!({
            "type": "attachment",
            "content": "x".repeat(3 * TAIL as usize),
        })
        .to_string();
        append(
            &path,
            &[assistant(), user("[Request interrupted by user]"), bulky],
        );
        assert!(interrupted(&path).is_some());
        assert_eq!(interrupted(&dir.path().join("missing.jsonl")), None);
    }
}
