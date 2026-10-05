//! The chat view of a Claude Code transcript,
//! `<config dir>/projects/<path_to_key(cwd)>/<session id>.jsonl`.
//!
//! The format is Claude Code's internal one and changes without notice;
//! what follows was verified on 2.1.284–2.1.289. A line pm does not
//! recognise reads as nothing, never as an error.
//!
//! - A `user` line whose content is a string is a prompt: typed
//!   (`promptSource: "typed"`, or `queued`), pm's Stop-hook continuation
//!   (`isMeta`, text `Stop hook feedback:…`), a background task's
//!   notification (`origin.kind: "task-notification"`), or the summary a
//!   compaction leaves (`isCompactSummary`). Other `isMeta` prompts (a
//!   skill's text) are context, not conversation.
//!   A long paste in a prompt is wrapped in `<pasted_content id=…>` tags,
//!   which are dropped.
//! - What the user typed while a turn ran is taken in at a step's end and
//!   recorded only as an `attachment` line (`queued_command`, origin
//!   `human`); one submitted after the turn is a `user` line like any.
//! - A `user` line whose content is blocks carries `tool_result`s, keyed by
//!   `tool_use_id`, or an interrupt (`[Request interrupted by user…]`).
//! - Each `assistant` line holds one content block; one API message spans
//!   several lines sharing `message.id`. An `isApiErrorMessage` line is a
//!   failed request, not a reply.
//! - `isSidechain` lines are a subagent's, hidden; subagents normally write
//!   to `<session id>/subagents/` instead.
//! - `system` lines are bookkeeping, but for `informational` notices and
//!   scheduled wake-ups.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::harness::AgentSession;
use crate::harness::transcript::items::{Body, Item, content_text, summarize_input, timestamp};
use crate::harness::transcript::jsonl::Entry;

/// Where Claude Code keeps the transcript of `agent`'s session: under the
/// config dir its environment named, else `~/.claude`, keyed by the
/// directory it runs in. `config_dir` is the one the session reported.
pub(in crate::harness) fn transcript_path(
    agent: &AgentSession<'_>,
    config_dir: Option<&Path>,
) -> Option<PathBuf> {
    if agent.session_id.is_empty() {
        return None;
    }
    let base = config_dir.map_or_else(|| agent.home.join(super::CONFIG_DIR), Path::to_path_buf);
    let path = base
        .join("projects")
        .join(super::sessions::path_to_key(agent.worktree))
        .join(format!("{}.jsonl", agent.session_id));
    path.is_file().then_some(path)
}

const CONTINUATION: &str = "Stop hook feedback:";
const INTERRUPTED: &str = "[Request interrupted by user";

pub(in crate::harness) fn parse(line: &Value, offset: u64) -> Vec<Entry> {
    if line.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }
    let at = timestamp(line.get("timestamp"));
    let id = line
        .get("uuid")
        .and_then(Value::as_str)
        .map_or_else(|| format!("@{offset}"), str::to_string);
    let id = id.as_str();
    let item = |body| vec![Entry::Item(Item::new(id, at, body))];
    match line.get("type").and_then(Value::as_str) {
        Some("user") => user(line, &item),
        Some("assistant") => assistant(line, id, at),
        Some("attachment") => queued(line, &item),
        Some("system") => match line.get("subtype").and_then(Value::as_str) {
            Some("informational" | "scheduled_task_fire") => {
                match line.get("content").and_then(Value::as_str) {
                    Some(text) if !text.is_empty() => item(Body::Event {
                        text: text.to_string(),
                    }),
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn user(line: &Value, item: &dyn Fn(Body) -> Vec<Entry>) -> Vec<Entry> {
    let flag = |key| line.get(key).and_then(Value::as_bool) == Some(true);
    let content = line.pointer("/message/content").unwrap_or(&Value::Null);
    if flag("isCompactSummary") {
        return item(Body::Compaction {
            summary: Some(content_text(content)),
        });
    }
    if let Value::Array(blocks) = content {
        let results: Vec<Entry> = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .filter_map(|b| {
                Some(Entry::Result {
                    call: b.get("tool_use_id")?.as_str()?.to_string(),
                    text: content_text(b.get("content").unwrap_or(&Value::Null)),
                    error: b.get("is_error").and_then(Value::as_bool) == Some(true),
                })
            })
            .collect();
        if !results.is_empty() {
            return results;
        }
    }
    let text = content_text(content);
    if text.starts_with(INTERRUPTED) {
        return item(Body::Event {
            text: "Interrupted".to_string(),
        });
    }
    if flag("isMeta") {
        return match text.strip_prefix(CONTINUATION) {
            Some(rest) => item(Body::Continuation {
                text: rest.trim().to_string(),
            }),
            None => Vec::new(),
        };
    }
    match line.pointer("/origin/kind").and_then(Value::as_str) {
        Some("task-notification") => item(Body::Event {
            text: tag(&text, "summary")
                .unwrap_or("Background task finished")
                .to_string(),
        }),
        Some("peer") => item(Body::Event {
            text: crate::harness::one_line(&text),
        }),
        _ if text.trim().is_empty() => Vec::new(),
        _ => item(Body::User {
            text: tag(&text, "command-name").map_or_else(|| unpasted(&text), str::to_string),
        }),
    }
}

/// What the user typed while a turn ran, which the turn takes in at a step's
/// end; it is recorded only as this attachment.
fn queued(line: &Value, item: &dyn Fn(Body) -> Vec<Entry>) -> Vec<Entry> {
    let attachment = line.get("attachment").unwrap_or(&Value::Null);
    let human = attachment.pointer("/origin/kind").and_then(Value::as_str) == Some("human");
    match attachment.get("prompt").and_then(Value::as_str) {
        Some(text)
            if human && attachment["type"] == "queued_command" && !text.trim().is_empty() =>
        {
            item(Body::User {
                text: unpasted(text),
            })
        }
        _ => Vec::new(),
    }
}

/// `text` with each paste Claude Code wrapped in `<pasted_content id=…>`
/// tags (a long one) put back as it was typed.
pub(crate) fn unpasted(text: &str) -> String {
    const OPEN: &str = "<pasted_content id=";
    const CLOSE: &str = "</pasted_content id=";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let Some(open_end) = rest[start..].find('>').map(|i| start + i + 1) else {
            break;
        };
        let id = &rest[start + OPEN.len()..open_end - 1];
        let close = format!("{CLOSE}{id}>");
        let Some(close_at) = rest[open_end..].find(&close).map(|i| open_end + i) else {
            break;
        };
        out.push_str(&rest[..start]);
        let inner = &rest[open_end..close_at];
        let inner = inner.strip_prefix('\n').unwrap_or(inner);
        out.push_str(inner.strip_suffix('\n').unwrap_or(inner));
        rest = &rest[close_at + close.len()..];
    }
    out.push_str(rest);
    out
}

fn assistant(line: &Value, id: &str, at: Option<chrono::DateTime<chrono::Utc>>) -> Vec<Entry> {
    let Some(Value::Array(blocks)) = line.pointer("/message/content") else {
        return Vec::new();
    };
    if line.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
        return vec![Entry::Item(Item::new(
            id,
            at,
            Body::Event {
                text: content_text(&Value::Array(blocks.clone())),
            },
        ))];
    }
    blocks
        .iter()
        .enumerate()
        .filter_map(|(i, block)| {
            let block_id = match i {
                0 => id.to_string(),
                i => format!("{id}:{i}"),
            };
            let text = |key| block.get(key).and_then(Value::as_str).unwrap_or_default();
            let body = match block.get("type").and_then(Value::as_str)? {
                "text" if !text("text").trim().is_empty() => Body::Assistant {
                    text: text("text").to_string(),
                },
                "thinking" if !text("thinking").trim().is_empty() => Body::Thinking {
                    text: text("thinking").to_string(),
                },
                "tool_use" => {
                    return Some(Entry::Item(Item::new(
                        block.get("id")?.as_str()?,
                        at,
                        Body::Tool {
                            name: text("name").to_string(),
                            input: summarize_input(block.get("input").unwrap_or(&Value::Null)),
                            result: None,
                        },
                    )));
                }
                _ => return None,
            };
            Some(Entry::Item(Item::new(block_id, at, body)))
        })
        .collect()
}

/// The text inside `<name>…</name>` in `text`.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(&format!("<{name}>"))?;
    let (inner, _) = rest.split_once(&format!("</{name}>"))?;
    Some(inner.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::transcript::jsonl;
    use std::path::Path;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/transcripts/claude-code.jsonl"
    );

    /// Each item's kind and the start of its text.
    fn rows(items: &[Item]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|item| {
                let json = serde_json::to_value(item).unwrap();
                let text = ["text", "input", "summary"]
                    .iter()
                    .find_map(|k| json[k].as_str())
                    .unwrap_or_default();
                (
                    json["kind"].as_str().unwrap().to_string(),
                    text.chars().take(30).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn a_real_transcript_reads_as_its_conversation() {
        let page = jsonl::page(Path::new(FIXTURE), None, 100, parse).unwrap();
        let rows = rows(&page.items);
        let expected = [
            ("user", "Stand by."),
            ("assistant", "Standing by. I'll start when a"),
            ("continuation", "You have new messages from no-"),
            ("tool", "pm msg read; pm workflow show"),
            ("thinking", "I confirmed `--add-dir=<dir>` "),
            ("event", "Background command \"Wait for T"),
            ("event", "Interrupted"),
            ("event", "API Error: Your computer went "),
            ("compaction", "This session is being continue"),
        ];
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(k, t)| (k.to_string(), t.to_string()))
            .collect();
        assert_eq!(rows, expected);
        let Body::Tool { result, name, .. } = &page.items[3].body else {
            panic!("not a tool: {:?}", page.items[3]);
        };
        assert_eq!(name, "Bash");
        let result = result
            .as_ref()
            .expect("the result on a later line is paired");
        assert!(result.text.starts_with("--- from no-reply-brief"));
        assert!(!result.error);
        assert_eq!(page.before, None, "the whole file was read");
    }

    #[test]
    fn what_the_user_typed_mid_turn_or_pasted_reads_as_typed() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/transcripts/claude-code-input.jsonl"
        );
        let page = jsonl::page(Path::new(fixture), None, 100, parse).unwrap();
        let texts: Vec<&str> = page
            .items
            .iter()
            .map(|item| match &item.body {
                Body::User { text } => text.as_str(),
                other => panic!("not the user's: {other:?}"),
            })
            .collect();
        let mut pasted = String::from("Count the lines below and reply with just the number.");
        for i in 1..=40 {
            pasted.push_str(&format!("\nline {i} of the log"));
        }
        assert_eq!(texts, ["say hi", pasted.as_str()]);
    }

    #[test]
    fn a_line_without_a_uuid_is_named_by_its_offset() {
        let line = serde_json::json!({"type": "user", "promptSource": "typed",
                                      "message": {"content": "hi"}});
        let [Entry::Item(item)] = &parse(&line, 42)[..] else {
            panic!("not one item");
        };
        assert_eq!(item.id, "@42");
    }

    #[test]
    fn a_subagents_line_is_hidden() {
        let line = serde_json::json!({
            "type": "assistant",
            "isSidechain": true,
            "uuid": "u",
            "message": {"content": [{"type": "text", "text": "hi"}]},
        });
        assert!(parse(&line, 0).is_empty());
    }
}
