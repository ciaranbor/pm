//! Codex's async questions (`request_user_input_async`, verified on 0.160),
//! answered remotely by typing codex's own reply into the composer.
//!
//! The tool returns at once and the question waits in the TUI, so no hook
//! holds a decision for it. Its `PreToolUse` carries the questions, each a
//! `title` and plain-string `options`, and `tool_use_id` the call id. The
//! TUI names question `i` of a call by the JSON array
//! `["request_user_input_async", <call id>, i]`, and takes as its answer a
//! prompt that is exactly a `<send_user_message_question_reply>` envelope
//! holding `{answer, question, questionItemId}` entries
//! (`context-fragments/src/answered_question.rs`, `tui/src/async_question_reply.rs`);
//! pasted into the composer and submitted, it answers just as the panel
//! does. Any prompt submitted from the composer, and the turn ending,
//! drop every question still pending. Only the root agent has the tool.

use serde_json::{Value, json};

use crate::state::runtime::{
    ANSWER_CHOICE, Answer, Choice, Dialog, DialogRecord, Question, QuestionOption, WaitingKind,
};

const TOOL: &str = "request_user_input_async";
const OPEN: &str = "<send_user_message_question_reply>";
const CLOSE: &str = "</send_user_message_question_reply>";
/// The longest question text codex puts in a reply.
const QUESTION_MAX: usize = 512;

/// The dialog an async question's `PreToolUse` opens, with its call id as
/// the reply context.
pub(in crate::harness) fn dialog(payload: &Value) -> Option<(Dialog, Value)> {
    if payload.get("hook_event_name")?.as_str()? != "PreToolUse"
        || payload.get("tool_name")?.as_str()? != TOOL
        || payload.get("agent_id").is_some()
    {
        return None;
    }
    let call_id = payload.get("tool_use_id")?.as_str()?;
    let questions: Vec<Question> = payload
        .pointer("/tool_input/questions")?
        .as_array()?
        .iter()
        .map(|q| {
            let options = q
                .get("options")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice);
            Some(Question {
                question: q.get("title")?.as_str()?.to_string(),
                header: String::new(),
                options: options
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|label| QuestionOption {
                        label: label.to_string(),
                        description: String::new(),
                    })
                    .collect(),
                multi_select: false,
                custom: true,
            })
        })
        .collect::<Option<_>>()?;
    let first = questions.first()?;
    let dialog = Dialog {
        detail: Some(crate::harness::one_line(&first.question)),
        choices: vec![Choice::new(ANSWER_CHOICE, "Submit", false)],
        questions,
        ..Dialog::new(WaitingKind::Question)
    };
    Some((dialog, json!({ "call_id": call_id })))
}

/// The prompt that answers `record`'s questions with `answer`, which it
/// accepts.
pub(in crate::harness) fn reply(record: &DialogRecord, answer: &Answer) -> String {
    let call_id = call_id(record).unwrap_or_default();
    let replies: Vec<Value> = record
        .dialog
        .questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let picked = answer
                .answers
                .get(&q.question)
                .and_then(|a| a.first())
                .map_or("", String::as_str);
            let end = q.question.floor_char_boundary(QUESTION_MAX);
            json!({
                "answer": picked,
                "question": q.question[..end].replace(['\n', '\r'], " "),
                "questionItemId": question_id(call_id, i),
            })
        })
        .collect();
    format!("{OPEN}\n{}\n{CLOSE}", Value::Array(replies))
}

/// Whether `prompt` is a reply answering a question of `record`'s.
pub(in crate::harness) fn answered_by(record: &DialogRecord, prompt: &str) -> bool {
    let Some(call_id) = call_id(record) else {
        return false;
    };
    let Some(body) = prompt
        .trim()
        .strip_prefix(OPEN)
        .and_then(|p| p.strip_suffix(CLOSE))
    else {
        return false;
    };
    let replies = match serde_json::from_str::<Value>(body) {
        Ok(Value::Array(replies)) => replies,
        Ok(one @ Value::Object(_)) => vec![one],
        _ => return false,
    };
    replies.iter().any(|r| {
        r.get("questionItemId")
            .and_then(Value::as_str)
            .and_then(|id| serde_json::from_str::<(String, String, usize)>(id).ok())
            .is_some_and(|(tool, id, _)| tool == TOOL && id == call_id)
    })
}

fn call_id(record: &DialogRecord) -> Option<&str> {
    record.reply_context.get("call_id")?.as_str()
}

fn question_id(call_id: &str, index: usize) -> String {
    json!([TOOL, call_id, index]).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload codex 0.160 sent for a live async question.
    fn asked() -> Value {
        json!({"hook_event_name": "PreToolUse", "tool_name": "request_user_input_async",
               "tool_use_id": "call_9Xk2",
               "tool_input": {"questions": [
                   {"title": "Which colour?", "options": ["Red", "Blue"]},
                   {"title": "Any notes?\nSay \"none\" if not"}]}})
    }

    fn record() -> DialogRecord {
        let (dialog, reply_context) = dialog(&asked()).unwrap();
        DialogRecord {
            dialog,
            pid: None,
            reply_context,
        }
    }

    #[test]
    fn an_async_question_opens_a_dialog_taking_options_or_words() {
        let r = record();
        let d = &r.dialog;
        assert_eq!(d.kind, WaitingKind::Question);
        assert_eq!(d.waiting().describe(), "Which colour?");
        let labels: Vec<&str> = d.questions[0]
            .options
            .iter()
            .map(|o| o.label.as_str())
            .collect();
        assert_eq!(labels, ["Red", "Blue"]);
        assert!(d.questions[1].options.is_empty());
        assert!(d.questions.iter().all(|q| q.custom && !q.multi_select));
        let answer = Answer {
            id: d.id.clone(),
            choice: ANSWER_CHOICE.into(),
            answers: [
                ("Which colour?".to_string(), vec!["Teal".to_string()]),
                (
                    "Any notes?\nSay \"none\" if not".to_string(),
                    vec!["none".to_string()],
                ),
            ]
            .into(),
            message: None,
        };
        assert_eq!(d.invalid(&answer), None);

        let subagents = {
            let mut p = asked();
            p["agent_id"] = json!("01a1");
            p
        };
        assert_eq!(dialog(&subagents), None);
        let plan_mode = json!({"hook_event_name": "PreToolUse", "tool_name": "request_user_input",
                               "tool_use_id": "c", "tool_input": {"questions": [{"title": "x"}]}});
        assert_eq!(dialog(&plan_mode), None);
    }

    #[test]
    fn the_reply_is_codexs_own_envelope_naming_each_question_by_call_and_index() {
        let r = record();
        let answer = Answer {
            id: r.dialog.id.clone(),
            choice: ANSWER_CHOICE.into(),
            answers: [
                ("Which colour?".to_string(), vec!["Blue".to_string()]),
                (
                    "Any notes?\nSay \"none\" if not".to_string(),
                    vec!["a \"quoted\" note".to_string()],
                ),
            ]
            .into(),
            message: None,
        };
        let reply = reply(&r, &answer);
        assert_eq!(
            reply,
            "<send_user_message_question_reply>\n\
             [{\"answer\":\"Blue\",\"question\":\"Which colour?\",\
             \"questionItemId\":\"[\\\"request_user_input_async\\\",\\\"call_9Xk2\\\",0]\"},\
             {\"answer\":\"a \\\"quoted\\\" note\",\"question\":\"Any notes? Say \\\"none\\\" if not\",\
             \"questionItemId\":\"[\\\"request_user_input_async\\\",\\\"call_9Xk2\\\",1]\"}]\n\
             </send_user_message_question_reply>"
        );
        assert!(answered_by(&r, &reply));
    }

    #[test]
    fn only_a_reply_naming_the_call_answers_it() {
        let r = record();
        // As codex's own panel submitted it, live.
        let panel = "<send_user_message_question_reply>\n[{\"answer\":\"Blue\",\
            \"question\":\"Which colour?\",\
            \"questionItemId\":\"[\\\"request_user_input_async\\\",\\\"call_9Xk2\\\",0]\"}]\n\
            </send_user_message_question_reply>";
        assert!(answered_by(&r, panel));
        let other_call = panel.replace("call_9Xk2", "call_other");
        assert!(!answered_by(&r, &other_call));
        assert!(!answered_by(&r, "Blue"));
        assert!(!answered_by(&r, &format!("{panel} and more")));
    }
}
