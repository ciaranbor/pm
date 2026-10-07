//! The chat view of one opencode `session_message` row (verified on
//! 2.0.18).
//!
//! - A `user` row typed in the TUI carries `files`; one the pm-never-idle
//!   plugin sent (or opencode itself, resuming an interrupted reply) does
//!   not, and is a continuation.
//! - An `assistant` row holds parts: `text`, `reasoning`, and `tool`, whose
//!   `state` carries both the call's `input` and its result (`content`, or
//!   `error`); the part's `time.completed` is when the call returned. A row
//!   with an `error` ended its turn on it.
//! - A `synthetic` row from `shell` reports a background command's end.
//! - A `compaction` row is a completed compaction with its `summary`, or a
//!   failed one with an `error`.
//! - `system` (instruction changes) and `idle` rows are bookkeeping.

use serde_json::Value;

use crate::harness::one_line;
use crate::harness::transcript::items::{
    Body, Item, ToolResult, content_text, summarize_input, timestamp,
};

/// A `session_message` row.
#[derive(Debug)]
pub(super) struct Row {
    pub id: String,
    pub kind: String,
    pub seq: i64,
    pub created: i64,
    pub updated: i64,
    pub data: String,
}

/// A tool part's output and whether it failed.
pub(super) fn tool_output(state: &Value) -> (String, bool) {
    match state.get("status").and_then(Value::as_str) {
        Some("error") => (
            state
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("failed")
                .to_string(),
            true,
        ),
        _ => (
            content_text(state.get("content").unwrap_or(&Value::Null)),
            false,
        ),
    }
}

pub(super) fn items(row: &Row) -> Vec<Item> {
    let Ok(data) = serde_json::from_str::<Value>(&row.data) else {
        return Vec::new();
    };
    let at = timestamp(data.pointer("/time/created"))
        .or_else(|| chrono::DateTime::from_timestamp_millis(row.created));
    let text = || {
        data.get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let one = |body| vec![Item::new(row.id.clone(), at, body)];
    match row.kind.as_str() {
        "user" if data.get("files").is_some() => one(Body::User { text: text() }),
        "user" => one(Body::Continuation { text: text() }),
        "assistant" => assistant(row, &data, at),
        "synthetic"
            if data.pointer("/metadata/source").and_then(Value::as_str) == Some("shell") =>
        {
            let state = data
                .pointer("/metadata/state")
                .and_then(Value::as_str)
                .unwrap_or("ended");
            let exit = data
                .pointer("/metadata/exit")
                .and_then(Value::as_i64)
                .map_or(String::new(), |code| format!(" (exit {code})"));
            one(Body::Event {
                text: format!("Background command {state}{exit}"),
                failure: false,
            })
        }
        "compaction" => match data.pointer("/error/message").and_then(Value::as_str) {
            Some(error) => one(Body::Event {
                text: format!("Compaction failed: {error}"),
                failure: true,
            }),
            None => one(Body::Compaction {
                summary: data
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            }),
        },
        _ => Vec::new(),
    }
}

fn assistant(row: &Row, data: &Value, at: Option<chrono::DateTime<chrono::Utc>>) -> Vec<Item> {
    let parts = data
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut out: Vec<Item> = parts
        .iter()
        .enumerate()
        .filter_map(|(i, part)| {
            let id = format!("{}:{i}", row.id);
            let at = timestamp(part.pointer("/time/created")).or(at);
            let text = part
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let body = match part.get("type").and_then(Value::as_str)? {
                "text" if !text.trim().is_empty() => Body::Assistant {
                    text: text.trim().to_string(),
                },
                "reasoning" if !text.trim().is_empty() => Body::Thinking { text },
                "tool" => {
                    let state = part.get("state").unwrap_or(&Value::Null);
                    let result = match state.get("status").and_then(Value::as_str) {
                        Some("completed" | "error") => {
                            let (output, error) = tool_output(state);
                            let ended = timestamp(part.pointer("/time/completed"));
                            Some(ToolResult::new(&output, error, ended, || id.clone()))
                        }
                        _ => None,
                    };
                    let tool_id = part.get("id").and_then(Value::as_str).unwrap_or(&id);
                    return Some(Item::new(
                        tool_id,
                        at,
                        Body::Tool {
                            name: part
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string(),
                            input: summarize_input(state.get("input").unwrap_or(&Value::Null)),
                            result,
                            unfinished: false,
                        },
                    ));
                }
                _ => return None,
            };
            Some(Item::new(id, at, body))
        })
        .collect();
    if let Some(error) = data.get("error") {
        let aborted = error.get("type").and_then(Value::as_str) == Some("aborted");
        let text = if aborted {
            "Interrupted".to_string()
        } else {
            one_line(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("The turn failed"),
            )
        };
        out.push(Item::new(
            format!("{}:error", row.id),
            at,
            Body::Event {
                text,
                failure: !aborted,
            },
        ));
    }
    out
}
