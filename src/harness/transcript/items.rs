//! The harness-neutral chat view of a conversation: what `pm serve` hands
//! the phone app, in the shape docs/remote-api.md's "Transcript contract"
//! publishes as a versioned contract. Every harness's reader produces
//! these and nothing else, so a change here is a change to that contract.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

/// The contract's version, bumped on any change a client could break on.
pub const VERSION: u32 = 1;

/// Tool output longer than this is cut, and served whole on request.
pub const RESULT_LIMIT: usize = 4 * 1024;

/// One row of the chat view. `id` is stable: a row read again — a tool call
/// once its result arrives, a message still being written — keeps its id,
/// and the client replaces the row it has.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub id: String,
    pub at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    /// Typed by the human.
    User {
        text: String,
    },
    /// The agent's reply, markdown.
    Assistant {
        text: String,
    },
    Thinking {
        text: String,
    },
    Tool {
        name: String,
        /// One line saying what the call was for.
        input: String,
        result: Option<ToolResult>,
        /// No result, and something other than a tool call followed it: it
        /// was interrupted, or its agent died mid-call. Set by
        /// [`mark_unfinished`]; a result arriving after all replaces the row.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        unfinished: bool,
    },
    /// A prompt pm's never-idle loop sent: a wake-up, not the human.
    Continuation {
        text: String,
    },
    /// The harness compacted the conversation's context.
    Compaction {
        summary: Option<String>,
    },
    /// Anything else worth a row of its own: an interrupt, a failed turn, a
    /// background task's end. `failure` marks one that says something went
    /// wrong (a failed request, turn or compaction), so a client can show it
    /// apart from bookkeeping.
    Event {
        text: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        failure: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolResult {
    pub text: String,
    pub error: bool,
    /// When the call returned, where the harness records it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
    pub truncated: bool,
    /// What fetches the whole output, when `text` was cut.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full: Option<String>,
}

impl ToolResult {
    /// `text` cut to [`RESULT_LIMIT`]; `full` names where the whole of it
    /// is read back from, kept only when something was cut.
    pub fn new(
        text: &str,
        error: bool,
        at: Option<DateTime<Utc>>,
        full: impl FnOnce() -> String,
    ) -> Self {
        if text.len() <= RESULT_LIMIT {
            return Self {
                text: text.to_string(),
                error,
                at,
                truncated: false,
                full: None,
            };
        }
        let mut cut = RESULT_LIMIT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        Self {
            text: text[..cut].to_string(),
            error,
            at,
            truncated: true,
            full: Some(full()),
        }
    }
}

impl Item {
    pub fn new(id: impl Into<String>, at: Option<DateTime<Utc>>, body: Body) -> Self {
        Self {
            id: id.into(),
            at,
            body,
        }
    }
}

/// A run of items read backwards from a cursor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Page {
    /// Oldest first.
    pub items: Vec<Item>,
    /// Where the next older page ends; `None` at the conversation's start.
    pub before: Option<String>,
    /// Where reading forward from the conversation's present end resumes.
    pub after: String,
}

/// What a conversation gained after a cursor.
#[derive(Debug, Clone, PartialEq)]
pub enum Tail {
    /// Items new or changed since, and the cursor after them.
    Items { items: Vec<Item>, after: String },
    /// The cursor no longer points into this conversation: its file was
    /// replaced or cut short.
    Reset,
}

/// Marks each call in `items` that has no result and that a later item
/// other than a call follows — in `items`, or after them when `followed` —
/// as unfinished. Calls side by side wait on their results together, so
/// only a turn, a reply or an event moving on leaves one unfinished.
pub fn mark_unfinished(items: &mut [Item], followed: bool) {
    let mut moved_on = followed;
    for item in items.iter_mut().rev() {
        match &mut item.body {
            Body::Tool {
                result: None,
                unfinished,
                ..
            } => *unfinished |= moved_on,
            Body::Tool { .. } => {}
            _ => moved_on = true,
        }
    }
}

/// Whether any of `items` is something other than a tool call.
pub fn moves_on(items: &[Item]) -> bool {
    items
        .iter()
        .any(|item| !matches!(item.body, Body::Tool { .. }))
}

/// A timestamp field as the harnesses write it: RFC 3339, or epoch
/// milliseconds.
pub fn timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> {
    match value? {
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        Value::Number(n) => DateTime::from_timestamp_millis(n.as_i64()?),
        _ => None,
    }
}

/// Keys whose value says what a tool call is for, most telling first: a
/// search's `pattern` or `query` says more than the `path` it searches.
const INPUT_KEYS: &[&str] = &[
    "command",
    "cmd",
    "file_path",
    "filePath",
    "pattern",
    "query",
    "path",
    "url",
    "skill",
    "description",
    "prompt",
];

/// One line for a tool call's `input`: its most telling field, else the
/// whole of it as compact JSON.
pub fn summarize_input(input: &Value) -> String {
    let text = match input {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(parsed @ Value::Object(_)) => return summarize_input(&parsed),
            _ => s.clone(),
        },
        Value::Object(map) => INPUT_KEYS
            .iter()
            .find_map(|key| match map.get(*key)? {
                Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
                Value::Array(words) => Some(
                    words
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                _ => None,
            })
            .unwrap_or_else(|| input.to_string()),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    crate::harness::one_line(&text)
}

/// The text of a content value: a string, or an array of blocks whose
/// `text` fields are joined and whose images are marked.
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                Value::String(s) => Some(s.clone()),
                _ if block.get("type").and_then(Value::as_str) == Some("image") => {
                    Some("[image]".to_string())
                }
                _ => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_item_serializes_flat_with_its_kind() {
        let item = Item::new(
            "t1",
            timestamp(Some(&json!("2026-10-02T18:08:51.608Z"))),
            Body::Tool {
                name: "Bash".into(),
                input: "ls".into(),
                result: Some(ToolResult::new(
                    "x".repeat(RESULT_LIMIT + 2).as_str(),
                    false,
                    timestamp(Some(&json!("2026-10-02T18:08:53.108Z"))),
                    || "ref".into(),
                )),
                unfinished: false,
            },
        );
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["kind"], "tool");
        assert_eq!(json["at"], "2026-10-02T18:08:51.608Z");
        assert_eq!(json["result"]["truncated"], true);
        assert_eq!(json["result"]["full"], "ref");
        assert_eq!(json["result"]["at"], "2026-10-02T18:08:53.108Z");
        assert_eq!(json["result"]["text"].as_str().unwrap().len(), RESULT_LIMIT);
    }

    #[test]
    fn an_event_says_it_is_a_failure_only_when_it_is_one() {
        let event = |failure| {
            serde_json::to_value(Item::new(
                "e",
                None,
                Body::Event {
                    text: "no errors found".into(),
                    failure,
                },
            ))
            .unwrap()
        };
        assert_eq!(event(true)["failure"], true);
        assert!(event(false).get("failure").is_none());
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        let text = format!("{}é", "x".repeat(RESULT_LIMIT - 1));
        let result = ToolResult::new(&text, false, None, String::new);
        assert_eq!(result.text.len(), RESULT_LIMIT - 1);
        assert!(result.truncated);
    }

    #[test]
    fn an_input_reads_as_its_most_telling_field() {
        assert_eq!(
            summarize_input(&json!({"description": "list", "command": "ls -la\npwd"})),
            "ls -la"
        );
        assert_eq!(
            summarize_input(&json!({"cmd": ["/bin/zsh", "-lc", "ls"]})),
            "/bin/zsh -lc ls"
        );
        assert_eq!(
            summarize_input(&json!("{\"cmd\":\"cargo test\",\"yield_time_ms\":1000}")),
            "cargo test"
        );
        assert_eq!(
            summarize_input(&json!({"path": "src", "pattern": "fn main", "glob": "*.rs"})),
            "fn main"
        );
        assert_eq!(summarize_input(&json!({"a": 1})), r#"{"a":1}"#);
    }
}
