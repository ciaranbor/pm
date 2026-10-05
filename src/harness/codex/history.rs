//! Steers seen in `$CODEX_HOME/history.jsonl` — a workaround standing in
//! for a codex hook that fires when input is steered or queued.
//!
//! codex holds text the user submits while pm's Stop hook waits, and runs
//! no hook for it until the Stop hook returns, so the hook cannot tell it
//! is there. The TUI does append each steer (text sent with Enter) to its
//! history file as it is submitted, as
//! `{"session_id":"<thread id>","ts":<unix s>,"text":"…"}` (verified on
//! 0.160), so the Stop hook watches that file for a line of its own
//! session and yields when one appears. A follow-up queued with Tab is not
//! written there and stays undetected. The line is appended asynchronously,
//! so a steer taken into the turn just before it ended can show up after
//! the hook starts; the Stop hook drops a steer the conversation already
//! holds, which is why the line's text and time are reported.
//!
//! Replace this once codex runs UserPromptSubmit, or a new event, as input
//! is steered or queued; each release's hook event list (`HookEventName`
//! in `codex-rs/protocol/src/protocol.rs`) shows when that has happened.
//!
//! The file is shared by every codex session on the machine. Only lines
//! appended after the watch starts count: the spawn prompt and text a
//! previous yield submitted are written there too. With
//! `history.max_bytes` set codex trims the file to 80% of the cap by
//! rewriting it; a file shorter than the bytes already read is rescanned
//! for lines dated from the watch's start. With `history.persistence =
//! "none"` nothing is written, and Esc remains the way to submit.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use serde::Deserialize;

use crate::harness::HeldText;

const HISTORY_FILE: &str = "history.jsonl";

#[derive(Deserialize)]
struct Line {
    session_id: String,
    ts: u64,
    text: String,
}

/// A watch on one session's steers in codex's history file.
#[derive(Debug)]
pub struct Steers {
    path: PathBuf,
    session_id: String,
    /// Unix seconds when the watch started.
    since: u64,
    /// Bytes of the file already read, up to the end of its last full line.
    read: u64,
}

impl Steers {
    /// Start watching `codex_home`'s history for `session_id`'s steers,
    /// counting none already written.
    pub fn watch(codex_home: &std::path::Path, session_id: &str) -> Self {
        let path = codex_home.join(HISTORY_FILE);
        let read = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            path,
            session_id: session_id.to_string(),
            since,
            read,
        }
    }

    /// The latest steer for the session written since the last check.
    /// Unreadable history reads as none.
    pub fn arrived(&mut self) -> Option<HeldText> {
        let len = std::fs::metadata(&self.path).map(|m| m.len()).ok()?;
        if len == self.read {
            return None;
        }
        let trimmed = len < self.read;
        let from = if trimmed { 0 } else { self.read };
        let appended = read_from(&self.path, from)?;
        let complete = appended.rfind('\n').map_or(0, |i| i + 1);
        self.read = from + complete as u64;
        appended[..complete]
            .lines()
            .filter_map(|line| serde_json::from_str::<Line>(line).ok())
            .rfind(|line| line.session_id == self.session_id && (!trimmed || line.ts >= self.since))
            .map(|line| HeldText {
                text: line.text,
                at: DateTime::from_timestamp(i64::try_from(line.ts).unwrap_or(i64::MAX), 0)
                    .unwrap_or_default(),
            })
    }
}

fn read_from(path: &std::path::Path, from: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn line(session: &str, ts: u64) -> String {
        format!("{{\"session_id\":\"{session}\",\"ts\":{ts},\"text\":\"hi\"}}\n")
    }

    fn append(dir: &std::path::Path, text: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(HISTORY_FILE))
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    fn text(steers: &mut Steers) -> Option<String> {
        steers.arrived().map(|held| held.text)
    }

    #[test]
    fn a_steer_for_the_session_written_during_the_watch_arrives_with_its_text_and_time() {
        let dir = tempdir().unwrap();
        append(dir.path(), &line("other", now()));
        let mut steers = Steers::watch(dir.path(), "me");
        assert_eq!(steers.arrived(), None);

        let ts = now();
        append(
            dir.path(),
            &format!("{{\"session_id\":\"me\",\"ts\":{ts},\"text\":\"deploy it\"}}\n"),
        );
        let held = steers.arrived().unwrap();
        assert_eq!(held.text, "deploy it");
        assert_eq!(held.at.timestamp(), ts as i64);
        assert_eq!(steers.arrived(), None, "a steer is seen once");
    }

    #[test]
    fn a_steer_for_the_session_in_a_history_created_during_the_watch_arrives() {
        let dir = tempdir().unwrap();
        let mut steers = Steers::watch(dir.path(), "me");
        assert_eq!(steers.arrived(), None);

        append(dir.path(), &line("me", now()));
        assert_eq!(text(&mut steers).as_deref(), Some("hi"));
    }

    #[test]
    fn another_sessions_steer_does_not_arrive() {
        let dir = tempdir().unwrap();
        let mut steers = Steers::watch(dir.path(), "me");

        append(dir.path(), &line("other", now()));
        assert_eq!(steers.arrived(), None);
    }

    #[test]
    fn a_steer_written_before_the_watch_does_not_arrive() {
        let dir = tempdir().unwrap();
        append(dir.path(), &line("me", now()));
        let mut steers = Steers::watch(dir.path(), "me");

        append(dir.path(), &line("other", now()));
        assert_eq!(steers.arrived(), None);
    }

    #[test]
    fn a_partly_written_line_arrives_once_complete() {
        let dir = tempdir().unwrap();
        let mut steers = Steers::watch(dir.path(), "me");
        let full = line("me", now());
        let (head, tail) = full.split_at(10);

        append(dir.path(), head);
        assert_eq!(steers.arrived(), None);
        append(dir.path(), tail);
        assert_eq!(text(&mut steers).as_deref(), Some("hi"));
    }

    #[test]
    fn a_trimmed_history_does_not_count_the_sessions_lines_from_before_the_watch() {
        let dir = tempdir().unwrap();
        let old = line("me", now() - 60);
        append(dir.path(), &old.repeat(20));
        let mut steers = Steers::watch(dir.path(), "me");

        std::fs::write(dir.path().join(HISTORY_FILE), old.repeat(2)).unwrap();
        assert_eq!(steers.arrived(), None);
    }

    #[test]
    fn a_trimmed_history_is_rescanned_for_a_steer_written_during_the_watch() {
        let dir = tempdir().unwrap();
        let old = line("me", now() - 60);
        append(dir.path(), &old.repeat(20));
        let mut steers = Steers::watch(dir.path(), "me");

        std::fs::write(
            dir.path().join(HISTORY_FILE),
            old.repeat(2) + &line("me", now()),
        )
        .unwrap();
        assert_eq!(text(&mut steers).as_deref(), Some("hi"));
    }
}
