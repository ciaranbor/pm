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
            let (text, error) = match exec_results(&text) {
                Some((outputs, failed)) => (outputs, error || failed),
                None => (text, error),
            };
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
    (!commands.is_empty()).then(|| crate::text::one_line(&commands.join("; ")))
}

/// What an `exec` script's result says its `exec_command` calls returned:
/// their outputs, joined, and whether one exited non-zero or was rejected.
/// The result is a `Script completed` header, then what the script
/// printed: JSON holding each call's `output` and, once it has exited, `exit_code`, bare or inside
/// `Promise.allSettled` entries; anything else is shown as printed.
/// `None` when it is not a script's result.
fn exec_results(text: &str) -> Option<(String, bool)> {
    let (header, printed) = text.split_once("Output:")?;
    if !header.trim_start().starts_with("Script completed") {
        return None;
    }
    let printed = printed.trim();
    let as_printed = || Some((printed.to_string(), false));
    let values: Vec<Value> = match serde_json::from_str(printed) {
        Ok(value) => vec![value],
        Err(_) => match printed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line.trim()).ok())
            .collect::<Option<_>>()
        {
            Some(values) => values,
            None => return as_printed(),
        },
    };
    let mut outputs = Vec::new();
    let mut failed = false;
    let mut found = false;
    let mut pending: Vec<&Value> = values.iter().rev().collect();
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(items) => pending.extend(items.iter().rev()),
            Value::Object(fields) => {
                if let Some(Value::String(out)) = fields.get("output") {
                    found = true;
                    failed |= fields
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .is_some_and(|code| code != 0);
                    outputs.push(out.trim_end().to_string());
                } else if fields.get("status").and_then(Value::as_str) == Some("rejected") {
                    found = true;
                    failed = true;
                    let reason = fields.get("reason").unwrap_or(&Value::Null);
                    outputs.push(content_text(reason));
                } else {
                    pending.extend(fields.values().rev());
                }
            }
            _ => {}
        }
    }
    if !found {
        return as_printed();
    }
    let outputs: Vec<String> = outputs.into_iter().filter(|o| !o.is_empty()).collect();
    Some((outputs.join("\n"), failed))
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
    if crate::messages::is_continuation(&text) {
        return item(Body::Continuation {
            text: text.trim().to_string(),
        });
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
        assert_eq!(result.text, "", "curl printed nothing");
        assert!(result.error, "curl exited 6");
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

    #[test]
    fn an_exec_result_reads_as_its_commands_output() {
        let printed = serde_json::json!([
            {"i": 0, "status": "fulfilled", "value": {"chunk_id": "a", "exit_code": 0,
                "output": "1\n2\n3\n"}},
            {"i": 1, "status": "fulfilled", "value": {"chunk_id": "b", "exit_code": 1,
                "output": "ls: /nonexistent-dir: No such file or directory\n"}},
        ]);
        let text = format!("Script completed\nWall time 0.1 seconds\nOutput:\n{printed}");
        assert_eq!(
            exec_results(&text),
            Some((
                "1\n2\n3\nls: /nonexistent-dir: No such file or directory".to_string(),
                true
            ))
        );
        let lines = "Script completed\nOutput:\n{\"exit_code\":0,\"output\":\"a\"}\n{\"exit_code\":0,\"output\":\"b\"}";
        assert_eq!(exec_results(lines), Some(("a\nb".to_string(), false)));
        let running = "Script completed\nOutput:\n{\"chunk_id\":\"c\",\"exit_code\":null,\"output\":\"so far\"}\n{\"output\":\"started\"}";
        assert_eq!(
            exec_results(running),
            Some(("so far\nstarted".to_string(), false))
        );
        let ok = "Script completed\nOutput:\n{\"exit_code\":0,\"output\":\"hi\\n\"}";
        assert_eq!(exec_results(ok), Some(("hi".to_string(), false)));
        assert_eq!(
            exec_results("Script completed\nWall time 0.1 seconds\nOutput:\n\n---\nname: x"),
            Some(("---\nname: x".to_string(), false))
        );
        assert_eq!(exec_results("plain words"), None);
    }

    #[test]
    fn a_queued_continuation_reads_as_one() {
        let line = serde_json::json!({"type": "response_item", "payload": {
            "type": "message", "id": "m", "role": "user",
            "content": [{"type": "input_text",
                         "text": "[pm message] From qa@x: \"done\". Run `pm msg read` to read it. \
                                  (pm delivers messages this way; this is not an error.)"}],
            "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["user.text"]}}});
        let [Entry::Item(item)] = &parse(&line, 0)[..] else {
            panic!("not one item");
        };
        assert!(matches!(item.body, Body::Continuation { .. }), "{item:?}");
    }
}
