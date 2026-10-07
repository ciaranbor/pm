//! Claude Code's dialogs as remote-answerable [`Dialog`]s, and an answer as
//! the `PermissionRequest` decision that applies it (verified on 2.1.289;
//! hooks reference: code.claude.com/docs/en/hooks.md).
//!
//! - In the interactive REPL the dialog shows at once and the hook runs
//!   beside it; whichever answers first wins. A decision printed after the
//!   user answered at the terminal is ignored and the hook isn't killed;
//!   Esc at the terminal sends it SIGTERM. A long-running hook delays
//!   neither later hooks nor the TUI.
//! - `AskUserQuestion` is answered by `allow` with `updatedInput` holding
//!   its input plus `answers`, question text → label; a bare `allow` is
//!   ignored for it. Multi-select labels are joined with `", "`; an answer
//!   is any string, never checked against the labels. A question whose
//!   `kind` is `text` or `number` (newer builds) has no options.
//! - `ExitPlanMode` (`{plan, planFilePath}`) is approved by `allow` echoing
//!   its input, which restores the mode from before planning; a `setMode`
//!   in `updatedPermissions` picks another. "Keep planning" is `deny` with
//!   a message. The CLI's "clear context" option has no hook equivalent.
//!   Its "use auto mode" choice is offered as the CLI offers it where auto
//!   mode is available; the payload doesn't say whether it is, and where it
//!   isn't, the CLI offers "auto-accept edits" instead — what `setMode`
//!   auto does there is unverified.
//! - A tool prompt's "don't ask again" is `allow` with the payload's
//!   `permission_suggestions` (allow rules and directories) as
//!   `updatedPermissions`; it is offered only when there are some, and the
//!   label approximates the CLI's. Its "switch to `<mode>`" is `allow` with
//!   the suggested `setMode`, offered only when one is suggested (a file
//!   write suggests `acceptEdits`, and the CLI offers it). The CLI's
//!   "switch to auto mode" on a Bash prompt comes with no suggestion, so it
//!   is left out: nothing in the payload says auto mode is available.
//! - The CLI's "No" denies and interrupts the turn; "No" with feedback
//!   (Tab to amend) denies with the feedback as a message and lets the
//!   agent carry on. A remote "No" does the same, by whether it has a
//!   message.

use serde_json::{Value, json};

use super::waiting::{plan_title, target};
use crate::harness::one_line;
use crate::state::runtime::{
    ANSWER_CHOICE, Answer, Choice, Dialog, DialogRecord, Question, QuestionOption, WaitingKind,
};

const DECLINE: &str = "decline";
const ALLOW: &str = "allow";
const ALWAYS: &str = "always";
const MODE: &str = "mode";
const DENY: &str = "deny";
const AUTO: &str = "auto";
const MANUAL: &str = "manual";
const KEEP_PLANNING: &str = "keep-planning";

const EVENT: &str = "PermissionRequest";
const QUESTION_TOOL: &str = "AskUserQuestion";
/// The hook events whose payloads open a dialog.
pub(in crate::harness) const EVENTS: &[&str] = &[EVENT];

/// The dialog a `PermissionRequest` payload opens, with what its decision
/// needs: the tool's input and the permission suggestions.
pub(in crate::harness) fn dialog(payload: &Value) -> Option<(Dialog, Value)> {
    if payload.get("hook_event_name")?.as_str()? != EVENT {
        return None;
    }
    let tool = payload.get("tool_name")?.as_str()?;
    let input = payload.get("tool_input").cloned().unwrap_or(json!({}));
    let mut dialog = match tool {
        QUESTION_TOOL => questions(&input)?,
        "ExitPlanMode" => {
            let plan = input.get("plan").and_then(Value::as_str).unwrap_or("");
            Dialog {
                detail: plan_title(plan),
                plan: Some(plan.to_string()),
                choices: vec![
                    Choice::new(AUTO, "Yes, and use auto mode", false),
                    Choice::new(MANUAL, "Yes, manually approve edits", false),
                    Choice::new(KEEP_PLANNING, "No, keep planning", true),
                ],
                ..Dialog::new(WaitingKind::Plan)
            }
        }
        _ => {
            let mut choices = vec![Choice::new(ALLOW, "Yes", false)];
            if let Some(label) = always_label(&remembered(payload)) {
                choices.push(Choice::new(ALWAYS, label, false));
            }
            if let Some(mode) = mode_switch(payload) {
                let mode = mode.get("mode").and_then(Value::as_str).unwrap_or("");
                let label = format!("Yes, and switch to {} for this session", mode_label(mode));
                choices.push(Choice::new(MODE, label, false));
            }
            choices.push(Choice::new(DENY, "No", true));
            Dialog {
                tool: Some(tool.to_string()),
                detail: target(&input).map(str::to_string),
                choices,
                ..Dialog::new(WaitingKind::Permission)
            }
        }
    };
    dialog.subagent = payload
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let suggestions = payload
        .get("permission_suggestions")
        .cloned()
        .unwrap_or(json!([]));
    Some((
        dialog,
        json!({"tool_name": tool, "tool_input": input, "permission_suggestions": suggestions}),
    ))
}

fn questions(input: &Value) -> Option<Dialog> {
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let questions: Vec<Question> = input
        .get("questions")?
        .as_array()?
        .iter()
        .map(|q| {
            let free = matches!(
                q.get("kind").and_then(Value::as_str),
                Some("text" | "number")
            );
            let options = q.get("options").and_then(Value::as_array);
            Question {
                question: text(q, "question"),
                header: text(q, "header"),
                options: options
                    .filter(|_| !free)
                    .into_iter()
                    .flatten()
                    .map(|o| QuestionOption {
                        label: text(o, "label"),
                        description: text(o, "description"),
                    })
                    .collect(),
                multi_select: !free && q.get("multiSelect").and_then(Value::as_bool) == Some(true),
                custom: true,
            }
        })
        .collect();
    if questions.is_empty() {
        return None;
    }
    Some(Dialog {
        detail: Some(one_line(&questions[0].question)),
        questions,
        choices: vec![
            Choice::new(ANSWER_CHOICE, "Submit answers", false),
            Choice::new(DECLINE, "Decline to answer", false),
        ],
        ..Dialog::new(WaitingKind::Question)
    })
}

/// The suggestions "don't ask again" applies: those that allow, not a
/// mode switch.
fn remembered(context: &Value) -> Vec<Value> {
    context
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| {
            s.get("type").and_then(Value::as_str) != Some("setMode")
                && s.get("behavior")
                    .and_then(Value::as_str)
                    .is_none_or(|b| b == "allow")
        })
        .cloned()
        .collect()
}

/// The mode switch a prompt suggests, if any.
fn mode_switch(context: &Value) -> Option<&Value> {
    context
        .get("permission_suggestions")?
        .as_array()?
        .iter()
        .find(|s| s.get("type").and_then(Value::as_str) == Some("setMode"))
}

/// The CLI's name for a permission mode.
fn mode_label(mode: &str) -> &str {
    match mode {
        "acceptEdits" => "accept edits",
        "auto" => "auto mode",
        "plan" => "plan mode",
        "default" => "default mode",
        "dontAsk" => "don't ask",
        "bypassPermissions" => "bypass permissions",
        other => other,
    }
}

fn always_label(suggestions: &[Value]) -> Option<String> {
    if suggestions.is_empty() {
        return None;
    }
    let strings = |s: &Value, key: &str| -> Vec<String> {
        s.get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let rules: Vec<String> = suggestions
        .iter()
        .flat_map(|s| {
            s.get("rules")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|r| {
            let tool = r.get("toolName")?.as_str()?;
            Some(match r.get("ruleContent").and_then(Value::as_str) {
                Some(content) => format!("{tool}({content})"),
                None => tool.to_string(),
            })
        })
        .collect();
    let dirs: Vec<String> = suggestions
        .iter()
        .flat_map(|s| strings(s, "directories"))
        .collect();
    let mut parts = Vec::new();
    if !rules.is_empty() {
        parts.push(format!("don't ask again for {}", rules.join(", ")));
    }
    if !dirs.is_empty() {
        parts.push(format!("always allow access to {}", dirs.join(", ")));
    }
    Some(match parts.is_empty() {
        true => "Yes, and don't ask again".to_string(),
        false => format!("Yes, and {}", parts.join(" and ")),
    })
}

/// Whether `payload` is the `PostToolUse` (or `PostToolUseFailure`) of
/// `record`'s tool call: the same thread, tool and input, as the payloads
/// carry no id that ties them. An `AskUserQuestion` answered at the
/// terminal comes back with its answers added to its input, so only its
/// questions are compared.
pub(in crate::harness) fn resolved(record: &DialogRecord, payload: &Value) -> bool {
    let event = payload.get("hook_event_name").and_then(Value::as_str);
    if !matches!(event, Some("PostToolUse" | "PostToolUseFailure")) {
        return false;
    }
    let context = &record.reply_context;
    let Some(tool) = context
        .get("tool_name")
        .filter(|t| payload.get("tool_name") == Some(t))
    else {
        return false;
    };
    let asked = context.get("tool_input");
    let done = payload.get("tool_input");
    let same_input = match tool.as_str() {
        Some(QUESTION_TOOL) => {
            asked.and_then(|i| i.get("questions")) == done.and_then(|i| i.get("questions"))
        }
        _ => asked == done,
    };
    payload.get("agent_id").and_then(Value::as_str) == record.dialog.subagent.as_deref()
        && same_input
}

/// The `PermissionRequest` output that applies `answer` to `record`'s
/// dialog, which [`Dialog::invalid`] has accepted it for.
pub(in crate::harness) fn decision(record: &DialogRecord, answer: &Answer) -> Value {
    let context = &record.reply_context;
    let input = context.get("tool_input").cloned().unwrap_or(json!({}));
    // With a message the agent is told why and carries on; without, a
    // denial that interrupts stops the turn.
    let deny = |interrupt: bool| match &answer.message {
        Some(message) => json!({"behavior": "deny", "message": message}),
        None if interrupt => json!({"behavior": "deny", "interrupt": true}),
        None => json!({"behavior": "deny"}),
    };
    let decision = match answer.choice.as_str() {
        ANSWER_CHOICE => {
            let answers: serde_json::Map<String, Value> = answer
                .answers
                .iter()
                .map(|(q, picked)| (q.clone(), json!(picked.join(", "))))
                .collect();
            let mut updated = input;
            updated["answers"] = Value::Object(answers);
            json!({"behavior": "allow", "updatedInput": updated})
        }
        DECLINE => json!({"behavior": "deny", "message": "The user declined to answer."}),
        AUTO => json!({
            "behavior": "allow",
            "updatedInput": input,
            "updatedPermissions": [
                {"type": "setMode", "mode": "auto", "destination": "session"}
            ],
        }),
        MANUAL => json!({"behavior": "allow", "updatedInput": input}),
        ALWAYS => json!({"behavior": "allow", "updatedPermissions": remembered(context)}),
        MODE => json!({"behavior": "allow", "updatedPermissions": [mode_switch(context)]}),
        ALLOW => json!({"behavior": "allow"}),
        KEEP_PLANNING => deny(false),
        _ => deny(true),
    };
    json!({
        "hookSpecificOutput": {"hookEventName": EVENT, "decision": decision}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn request(tool: &str, input: Value) -> Value {
        json!({"hook_event_name": "PermissionRequest", "tool_name": tool, "tool_input": input})
    }

    fn record(payload: &Value) -> DialogRecord {
        let (dialog, reply_context) = dialog(payload).unwrap();
        DialogRecord {
            dialog,
            pid: Some(1),
            reply_context,
        }
    }

    fn answer(record: &DialogRecord, choice: &str, answers: &[(&str, &[&str])]) -> Answer {
        Answer {
            id: record.dialog.id.clone(),
            choice: choice.into(),
            answers: answers
                .iter()
                .map(|(q, a)| (q.to_string(), a.iter().map(|s| s.to_string()).collect()))
                .collect::<BTreeMap<_, _>>(),
            message: None,
        }
    }

    fn decided(record: &DialogRecord, answer: &Answer) -> Value {
        let out = decision(record, answer);
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        out["hookSpecificOutput"]["decision"].clone()
    }

    fn ids(dialog: &Dialog) -> Vec<&str> {
        dialog.choices.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn questions_are_answered_with_their_input_echoed_and_labels_joined() {
        let input = json!({"questions": [
            {"question": "Which DB?", "header": "DB", "multiSelect": false,
             "options": [{"label": "Postgres", "description": "server"},
                         {"label": "SQLite", "description": "file"}]},
            {"question": "Which features?", "header": "Features", "multiSelect": true,
             "options": [{"label": "Auth", "description": ""},
                         {"label": "Search", "description": ""}]},
            {"question": "How many users?", "header": "Users", "kind": "number",
             "options": [{"label": "ignored", "description": ""}]}
        ], "metadata": {"source": "x"}});
        let r = record(&json!({
            "hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
            "tool_input": input, "agent_id": "a1"
        }));
        let d = &r.dialog;
        assert_eq!(d.kind, WaitingKind::Question);
        assert_eq!(d.subagent.as_deref(), Some("a1"));
        assert_eq!(d.detail.as_deref(), Some("Which DB?"));
        assert_eq!(ids(d), [ANSWER_CHOICE, DECLINE]);
        assert!(!d.questions[0].multi_select && d.questions[1].multi_select);
        assert_eq!(d.questions[0].options[1].label, "SQLite");
        assert!(d.questions[2].options.is_empty(), "a number question");
        assert!(d.questions.iter().all(|q| q.custom));

        let a = answer(
            &r,
            ANSWER_CHOICE,
            &[
                ("Which DB?", &["SQLite"]),
                ("Which features?", &["Auth", "Search"]),
                ("How many users?", &["about 40"]),
            ],
        );
        assert_eq!(d.invalid(&a), None);
        let mut expected = input.clone();
        expected["answers"] = json!({
            "Which DB?": "SQLite",
            "Which features?": "Auth, Search",
            "How many users?": "about 40",
        });
        assert_eq!(
            decided(&r, &a),
            json!({"behavior": "allow", "updatedInput": expected})
        );
        assert_eq!(decided(&r, &answer(&r, DECLINE, &[]))["behavior"], "deny");
    }

    #[test]
    fn a_tool_prompt_offers_the_suggested_rules_and_mode_and_no_interrupts_unless_told_why() {
        let bare = record(&request("Bash", json!({"command": "rm -rf build"})));
        assert_eq!(bare.dialog.kind, WaitingKind::Permission);
        assert_eq!(bare.dialog.tool.as_deref(), Some("Bash"));
        assert_eq!(bare.dialog.detail.as_deref(), Some("rm -rf build"));
        assert_eq!(ids(&bare.dialog), [ALLOW, DENY]);
        assert_eq!(
            decided(&bare, &answer(&bare, DENY, &[])),
            json!({"behavior": "deny", "interrupt": true})
        );

        let rule = json!({"type": "addRules", "behavior": "allow", "destination": "localSettings",
                          "rules": [{"toolName": "Bash", "ruleContent": "npm test:*"}]});
        let mode = json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"});
        let mut payload = request("Bash", json!({"command": "npm test"}));
        payload["permission_suggestions"] = json!([rule, mode]);
        let r = record(&payload);
        assert_eq!(ids(&r.dialog), [ALLOW, ALWAYS, MODE, DENY]);
        assert_eq!(
            r.dialog.choices[1].label,
            "Yes, and don't ask again for Bash(npm test:*)"
        );
        assert_eq!(
            r.dialog.choices[2].label,
            "Yes, and switch to accept edits for this session"
        );
        assert_eq!(
            decided(&r, &answer(&r, ALWAYS, &[])),
            json!({"behavior": "allow", "updatedPermissions": [rule]})
        );
        assert_eq!(
            decided(&r, &answer(&r, MODE, &[])),
            json!({"behavior": "allow", "updatedPermissions": [mode]})
        );
        assert_eq!(
            decided(&r, &answer(&r, ALLOW, &[])),
            json!({"behavior": "allow"})
        );

        let mut no = answer(&r, DENY, &[]);
        no.message = Some("use cargo instead".into());
        assert_eq!(
            decided(&r, &no),
            json!({"behavior": "deny", "message": "use cargo instead"})
        );

        let mut only_mode = request("Write", json!({"file_path": "/a"}));
        only_mode["permission_suggestions"] = json!([mode]);
        assert_eq!(ids(&record(&only_mode).dialog), [ALLOW, MODE, DENY]);
    }

    #[test]
    fn only_the_same_threads_call_of_the_same_tool_and_input_resolves_a_dialog() {
        let input = json!({"command": "cargo test"});
        let mut payload = request("Bash", input.clone());
        payload["agent_id"] = json!("a1");
        let r = record(&payload);
        let post = |event: &str, agent: Option<&str>, tool: &str, input: &Value| {
            let mut p = json!({"hook_event_name": event, "tool_name": tool,
                               "tool_input": input, "tool_use_id": "t"});
            if let Some(agent) = agent {
                p["agent_id"] = json!(agent);
            }
            resolved(&r, &p)
        };
        assert!(post("PostToolUse", Some("a1"), "Bash", &input));
        assert!(post("PostToolUseFailure", Some("a1"), "Bash", &input));
        assert!(
            !post("PostToolUse", None, "Bash", &input),
            "the main thread"
        );
        assert!(!post("PostToolUse", Some("a2"), "Bash", &input));
        assert!(!post(
            "PostToolUse",
            Some("a1"),
            "Bash",
            &json!({"command": "ls"})
        ));
        assert!(!post("PostToolUse", Some("a1"), "Read", &input));
        assert!(!post("PermissionRequest", Some("a1"), "Bash", &input));
    }

    #[test]
    fn a_plan_is_approved_with_or_without_a_mode_or_sent_back() {
        let input = json!({"plan": "# Add login\n- form", "planFilePath": "/p/plan.md"});
        let r = record(&request("ExitPlanMode", input.clone()));
        assert_eq!(r.dialog.kind, WaitingKind::Plan);
        assert_eq!(r.dialog.detail.as_deref(), Some("Add login"));
        assert_eq!(r.dialog.plan.as_deref(), Some("# Add login\n- form"));
        assert_eq!(ids(&r.dialog), [AUTO, MANUAL, KEEP_PLANNING]);

        assert_eq!(
            decided(&r, &answer(&r, MANUAL, &[])),
            json!({"behavior": "allow", "updatedInput": input})
        );
        assert_eq!(
            decided(&r, &answer(&r, AUTO, &[])),
            json!({"behavior": "allow", "updatedInput": input, "updatedPermissions": [
                {"type": "setMode", "mode": "auto", "destination": "session"}]})
        );
        assert_eq!(
            decided(&r, &answer(&r, KEEP_PLANNING, &[])),
            json!({"behavior": "deny"}),
            "keeps planning: no interrupt"
        );
        let mut keep = answer(&r, KEEP_PLANNING, &[]);
        keep.message = Some("split step 2".into());
        assert_eq!(
            decided(&r, &keep),
            json!({"behavior": "deny", "message": "split step 2"})
        );
    }

    #[test]
    fn other_events_open_no_dialog() {
        assert!(dialog(&json!({"hook_event_name": "PostToolUse", "tool_name": "Bash"})).is_none());
        assert!(
            dialog(&request("AskUserQuestion", json!({"questions": []}))).is_none(),
            "no questions to answer"
        );
    }
}
