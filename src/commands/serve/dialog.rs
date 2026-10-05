//! `…/dialog`: the dialog on an agent's screen that can be answered
//! remotely, and its answer, which the dialog's hook hands its harness
//! ([`hooks_dialog`]). The answer is logged by its choice only, never the
//! words typed.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;

use crate::commands::agent_input;
use crate::commands::hooks_dialog::{self, Answered};
use crate::error::Result;
use crate::state::runtime::Answer;

use super::input::{MAX_TEXT, Written};
use super::routes::{Reply, error, json};
use super::transcript::Agent;

/// How long an answer waits for the dialog's hook to take it.
const TAKEN_WITHIN: Duration = Duration::from_secs(5);

const NONE: &str = "no dialog of the agent's can be answered";

/// `GET …/dialog`.
pub(super) fn get(agent: &Agent) -> Result<Reply> {
    let Some(harness) = agent.harness()? else {
        return Ok(error(404, NONE));
    };
    Ok(
        match hooks_dialog::current(&agent.root, &agent.scope, &agent.name, harness) {
            Some(record) => json(200, serde_json::to_value(&record.dialog)?),
            None => error(404, NONE),
        },
    )
}

#[derive(Deserialize)]
struct Body {
    id: String,
    choice: String,
    #[serde(default)]
    answers: BTreeMap<String, Picked>,
    message: Option<String>,
}

/// A question's answer: one label or the user's words, or a multi-select's
/// several.
#[derive(Deserialize)]
#[serde(untagged)]
enum Picked {
    One(String),
    Several(Vec<String>),
}

/// `POST …/dialog`.
pub(super) fn post(agent: &Agent, body: &str) -> Result<Written> {
    let body: Body = match serde_json::from_str(body) {
        Ok(body) => body,
        Err(e) => return Ok(bad(&format!("the body is not a dialog answer: {e}"))),
    };
    let message = body.message.filter(|m| !m.trim().is_empty());
    let mut detail = format!("dialog {}", body.choice);
    if let Some(message) = &message {
        if message.len() > MAX_TEXT {
            return Ok(bad(&format!("message is over {MAX_TEXT} bytes")));
        }
        detail.push_str(&format!(" message sha256:{}", agent_input::sha256(message)));
    }
    let answer = Answer {
        id: body.id,
        choice: body.choice,
        answers: body
            .answers
            .into_iter()
            .map(|(q, picked)| match picked {
                Picked::One(one) => (q, vec![one]),
                Picked::Several(several) => (q, several),
            })
            .collect(),
        message,
    };
    let Some(harness) = agent.harness()? else {
        return Ok(Written {
            reply: refused("answered", "the agent is gone"),
            detail,
        });
    };
    let reply = match hooks_dialog::answer(
        &agent.root,
        &agent.scope,
        &agent.name,
        harness,
        &answer,
        TAKEN_WITHIN,
    )? {
        Answered::Taken => json(200, serde_json::json!({ "answered": true })),
        Answered::Invalid(why) => error(400, &why),
        Answered::Elsewhere => refused(
            "answered",
            "the dialog is no longer up: answered at the terminal, or replaced",
        ),
        Answered::Gone => refused("gone", "the dialog's hook is gone"),
    };
    Ok(Written { reply, detail })
}

fn bad(message: &str) -> Written {
    Written {
        reply: error(400, message),
        detail: String::new(),
    }
}

fn refused(code: &str, message: &str) -> Reply {
    json(
        409,
        serde_json::json!({ "error": message, "refused": code }),
    )
}
