//! The chat view of a codex rollout,
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*-<session id>.jsonl`.
//!
//! The conversation is read from the `response_item` lines, the record codex
//! resumes from (verified on 0.160). `event_msg` lines mostly repeat them
//! for a UI, and only a turn's failure (`task_complete` with an `error`)
//! and an interrupt (`turn_aborted`) are read from those.
//!
//! - A `user` message is typed when its metadata's `content_item_kinds`
//!   holds `user.text`; a Stop hook's continuation is wrapped in
//!   `<hook_prompt …>`; anything else is context codex injected
//!   (`<environment_context>`, `AGENTS.md`, plugin lists…). A rollout
//!   without that metadata is read by the wrapping alone: context is a tag
//!   or headed `# AGENTS.md instructions`. `developer` messages are
//!   instructions, never shown.
//! - A tool call (`function_call`, `custom_tool_call`, `local_shell_call`)
//!   and its output (`…_output`) share a `call_id`.
//! - `reasoning` is shown only through its `summary`; its content is
//!   encrypted.
//! - A `compacted` line marks a compaction.

use serde_json::Value;

use crate::harness::transcript::items::{Body, Item, content_text, summarize_input, timestamp};
use crate::harness::transcript::jsonl::Entry;

const HOOK_PROMPT: &str = "<hook_prompt";
const AGENTS_MD: &str = "# AGENTS.md instructions";

pub(in crate::harness) fn parse(line: &Value, offset: u64) -> Vec<Entry> {
    let at = timestamp(line.get("timestamp"));
    let Some(payload) = line.get("payload") else {
        return Vec::new();
    };
    let field = |key| payload.get(key).and_then(Value::as_str);
    let id = field("id").map_or_else(|| format!("@{offset}"), str::to_string);
    let item = |body| vec![Entry::Item(Item::new(id.clone(), at, body))];
    match (line.get("type").and_then(Value::as_str), field("type")) {
        (Some("response_item"), Some("message")) => {
            let text = content_text(payload.get("content").unwrap_or(&Value::Null));
            match field("role") {
                Some("assistant") if !text.trim().is_empty() => item(Body::Assistant { text }),
                Some("user") => user(text, kinds(payload), &item),
                _ => Vec::new(),
            }
        }
        (Some("response_item"), Some("reasoning")) => {
            let text = content_text(payload.get("summary").unwrap_or(&Value::Null));
            if text.trim().is_empty() {
                Vec::new()
            } else {
                item(Body::Thinking { text })
            }
        }
        (
            Some("response_item"),
            Some(kind @ ("function_call" | "custom_tool_call" | "local_shell_call")),
        ) => {
            let Some(call) = field("call_id") else {
                return Vec::new();
            };
            let input = match kind {
                "function_call" => payload.get("arguments"),
                "custom_tool_call" => payload.get("input"),
                _ => payload.pointer("/action/command"),
            };
            let name = field("name").unwrap_or(match kind {
                "local_shell_call" => "shell",
                _ => "tool",
            });
            vec![Entry::Item(Item::new(
                call,
                at,
                Body::Tool {
                    name: name.to_string(),
                    input: input
                        .and_then(Value::as_str)
                        .and_then(script_commands)
                        .unwrap_or_else(|| summarize_input(input.unwrap_or(&Value::Null))),
                    result: None,
                    unfinished: false,
                },
            ))]
        }
        (
            Some("response_item"),
            Some("function_call_output" | "custom_tool_call_output" | "local_shell_call_output"),
        ) => {
            let Some(call) = field("call_id") else {
                return Vec::new();
            };
            let output = payload.get("output").unwrap_or(&Value::Null);
            let text = match output.get("content") {
                Some(content) => content_text(content),
                None => content_text(output),
            };
            let error = output.get("success").and_then(Value::as_bool) == Some(false);
            vec![Entry::Result {
                call: call.to_string(),
                text,
                error,
                at,
            }]
        }
        (Some("event_msg"), Some("task_complete")) => {
            match payload.get("error").filter(|e| !e.is_null()) {
                Some(error) => item(Body::Event {
                    text: super::transcript::describe(error),
                    failure: true,
                }),
                None => Vec::new(),
            }
        }
        (Some("event_msg"), Some("turn_aborted")) => item(Body::Event {
            text: "Interrupted".to_string(),
            failure: false,
        }),
        (Some("compacted"), _) => item(Body::Compaction {
            summary: field("message")
                .filter(|m| !m.trim().is_empty())
                .map(str::to_string),
        }),
        _ => Vec::new(),
    }
}

/// The shell commands a script for codex's `exec` tool runs, joined:
/// each is the string literal after `cmd:` in an `exec_command` call.
fn script_commands(script: &str) -> Option<String> {
    let commands: Vec<String> = script
        .split("exec_command(")
        .skip(1)
        .filter_map(|call| {
            let rest = call.trim_start().strip_prefix('{')?.trim_start();
            let literal = rest.strip_prefix("cmd:")?.trim_start();
            let mut end = None;
            let mut escaped = false;
            for (at, c) in literal.char_indices().skip(1) {
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => {
                        end = Some(at);
                        break;
                    }
                    _ => {}
                }
            }
            serde_json::from_str::<String>(&literal[..=end?]).ok()
        })
        .collect();
    (!commands.is_empty()).then(|| crate::harness::one_line(&commands.join("; ")))
}

/// What codex says a message's content is (`user.text` when typed), where
/// it says.
fn kinds(payload: &Value) -> Option<Vec<&str>> {
    let kinds =
        payload.pointer("/internal_chat_message_metadata_passthrough/content_item_kinds")?;
    Some(kinds.as_array()?.iter().filter_map(Value::as_str).collect())
}

fn user(text: String, kinds: Option<Vec<&str>>, item: &dyn Fn(Body) -> Vec<Entry>) -> Vec<Entry> {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix(HOOK_PROMPT) {
        let inner = rest.split_once('>').map_or(rest, |(_, inner)| inner);
        let inner = inner.trim_end().trim_end_matches("</hook_prompt>");
        return item(Body::Continuation {
            text: inner.trim().to_string(),
        });
    }
    let typed = match kinds {
        Some(kinds) => kinds.contains(&"user.text"),
        None => !trimmed.starts_with('<') && !trimmed.starts_with(AGENTS_MD),
    };
    if !typed || trimmed.is_empty() {
        return Vec::new();
    }
    item(Body::User { text })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::transcript::jsonl;
    use std::path::Path;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/transcripts/codex.jsonl"
    );

    #[test]
    fn a_real_rollout_reads_as_its_conversation() {
        let page = jsonl::page(Path::new(FIXTURE), None, 100, parse).unwrap();
        let rows: Vec<(String, String)> = page
            .items
            .iter()
            .map(|item| {
                let json = serde_json::to_value(item).unwrap();
                let text = json["text"].as_str().or(json["input"].as_str());
                (
                    json["kind"].as_str().unwrap().to_string(),
                    text.unwrap_or_default().chars().take(24).collect(),
                )
            })
            .collect();
        let expected = [
            ("user", "Reply with only the word"),
            ("assistant", "pong"),
            ("user", "Reply with only the word"),
            ("assistant", "ping0.1"),
            ("user", "Reply with only the word"),
            ("assistant", "ping0.3"),
            ("user", "Run exactly this shell c"),
            ("assistant", "I’ll run that command."),
            ("tool", "curl -sI https://example"),
            ("tool", "curl -sI https://example"),
            ("event", "Interrupted"),
            ("continuation", "You have new messages fr"),
            ("assistant", "I’ll check the feature w"),
        ];
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(k, t)| (k.to_string(), t.to_string()))
            .collect();
        assert_eq!(rows, expected);
        let Body::Tool {
            result: Some(result),
            name,
            ..
        } = &page.items[8].body
        else {
            panic!("no result: {:?}", page.items[8]);
        };
        assert_eq!(name, "exec");
        assert!(
            result.text.starts_with("Script completed"),
            "{}",
            result.text
        );
    }

    #[test]
    fn an_exec_script_reads_as_the_commands_it_runs() {
        let script = r#"const r = await Promise.allSettled([tools.exec_command({cmd:"pm msg read",max_output_tokens:4000}), tools.exec_command({ cmd: "echo \"hi\"" })]);"#;
        assert_eq!(
            script_commands(script).as_deref(),
            Some(r#"pm msg read; echo "hi""#)
        );
        assert_eq!(script_commands("text(1)"), None);
    }

    #[test]
    fn a_failed_turn_reads_as_an_event() {
        let line = serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "task_complete", "turn_id": "t1",
                        "error": {"message": "stream disconnected"}},
        });
        let entries = parse(&line, 7);
        let [Entry::Item(item)] = &entries[..] else {
            panic!("not one item");
        };
        assert_eq!(item.id, "@7", "a line without an id is named by its offset");
        assert_eq!(
            item.body,
            Body::Event {
                text: "API error: stream disconnected".into(),
                failure: true,
            }
        );
    }
}
