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
//!   label approximates the CLI's. Its "switch to auto mode" or "switch to
//!   accept edits" is left out: which one the CLI shows doesn't follow the
//!   suggestions (a Bash prompt suggested `acceptEdits` while the CLI
//!   offered auto mode).

use serde_json::{Value, json};

use super::waiting::{plan_title, target};
use crate::harness::one_line;
use crate::state::runtime::{
    ANSWER_CHOICE, Answer, Choice, Dialog, DialogRecord, Question, QuestionOption, WaitingKind,
};

const DECLINE: &str = "decline";
const ALLOW: &str = "allow";
const ALWAYS: &str = "always";
const DENY: &str = "deny";
const AUTO: &str = "auto";
const MANUAL: &str = "manual";
const KEEP_PLANNING: &str = "keep-planning";

/// The dialog a `PermissionRequest` payload opens, with what its decision
/// needs: the tool's input and the permission suggestions.
pub(in crate::harness) fn dialog(payload: &Value) -> Option<(Dialog, Value)> {
    if payload.get("hook_event_name")?.as_str()? != "PermissionRequest" {
        return None;
    }
    let tool = payload.get("tool_name")?.as_str()?;
    let input = payload.get("tool_input").cloned().unwrap_or(json!({}));
    let mut dialog = match tool {
        "AskUserQuestion" => questions(&input)?,
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
        json!({"tool_input": input, "permission_suggestions": suggestions}),
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

/// The `PermissionRequest` output that applies `answer` to `record`'s
/// dialog, which [`Dialog::invalid`] has accepted it for.
pub(in crate::harness) fn decision(record: &DialogRecord, answer: &Answer) -> Value {
    let context = &record.reply_context;
    let input = context.get("tool_input").cloned().unwrap_or(json!({}));
    let deny = || {
        let mut d = json!({"behavior": "deny"});
        if let Some(message) = &answer.message {
            d["message"] = json!(message);
        }
        d
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
        ALLOW => json!({"behavior": "allow"}),
        _ => deny(),
    };
    json!({
        "hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}
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
            pid: 1,
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
    fn dont_ask_again_is_offered_only_with_suggestions_and_applies_them() {
        let bare = record(&request("Bash", json!({"command": "rm -rf build"})));
        assert_eq!(bare.dialog.kind, WaitingKind::Permission);
        assert_eq!(bare.dialog.tool.as_deref(), Some("Bash"));
        assert_eq!(bare.dialog.detail.as_deref(), Some("rm -rf build"));
        assert_eq!(ids(&bare.dialog), [ALLOW, DENY]);

        let rule = json!({"type": "addRules", "behavior": "allow", "destination": "localSettings",
                          "rules": [{"toolName": "Bash", "ruleContent": "npm test:*"}]});
        let mode = json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"});
        let mut payload = request("Bash", json!({"command": "npm test"}));
        payload["permission_suggestions"] = json!([rule, mode]);
        let r = record(&payload);
        assert_eq!(ids(&r.dialog), [ALLOW, ALWAYS, DENY]);
        assert_eq!(
            r.dialog.choices[1].label,
            "Yes, and don't ask again for Bash(npm test:*)"
        );
        assert_eq!(
            decided(&r, &answer(&r, ALWAYS, &[])),
            json!({"behavior": "allow", "updatedPermissions": [rule]})
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
        assert_eq!(ids(&record(&only_mode).dialog), [ALLOW, DENY]);
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
