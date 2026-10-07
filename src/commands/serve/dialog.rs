//! `…/dialog` and `…/dialogs`: the dialogs on an agent's screen that can
//! be answered remotely, and an answer to one, which the dialog's hook
//! hands its harness, or which is typed as the harness's own reply to a
//! dialog no hook holds ([`hooks_dialog`]). The answer is logged by its choice
//! only, never the words typed.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;

use crate::commands::agent_input;
use crate::commands::hooks_dialog::{self, Answered};
use crate::error::Result;
use crate::state::runtime::{Answer, Dialog};

use super::input::{MAX_TEXT, Written};
use super::routes::{Reply, error, json};
use super::transcript::Agent;

/// How long an answer waits for the dialog's hook to take it.
const TAKEN_WITHIN: Duration = Duration::from_secs(5);

const NONE: &str = "no dialog of the agent's can be answered";

/// `GET …/dialog`, the oldest open dialog, which is the one a terminal
/// queueing several shows; or `GET …/dialogs`, every one, oldest first.
pub(super) fn get(agent: &Agent, all: bool) -> Result<Reply> {
    let open: Vec<Dialog> = match agent.harness()? {
        Some(harness) => {
            hooks_dialog::open_dialogs(&agent.root, &agent.scope, &agent.name, harness)
                .into_iter()
                .map(|r| r.dialog)
                .collect()
        }
        None => Vec::new(),
    };
    if all {
        return Ok(json(200, serde_json::json!({ "dialogs": open })));
    }
    Ok(match open.first() {
        Some(oldest) => json(200, serde_json::to_value(oldest)?),
        None => error(404, NONE),
    })
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
pub(super) fn post(agent: &Agent, body: &str, tmux_server: Option<&str>) -> Result<Written> {
    let body: Body = match serde_json::from_str(body) {
        Ok(body) => body,
        Err(e) => return Ok(bad(&format!("the body is not a dialog answer: {e}"))),
    };
    let message = body.message.filter(|m| !m.trim().is_empty());
    if message.as_ref().is_some_and(|m| m.len() > MAX_TEXT) {
        return Ok(bad(&format!("message is over {MAX_TEXT} bytes")));
    }
    // Logged only once the dialog has accepted the choice.
    let mut detail = format!("dialog {}", body.choice);
    if let Some(message) = &message {
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
            reply: error(404, "no such agent"),
            detail: String::new(),
        });
    };
    let answered = hooks_dialog::answer(
        &agent.root,
        &agent.scope,
        &agent.name,
        harness,
        &answer,
        TAKEN_WITHIN,
        tmux_server,
    )?;
    let (reply, detail) = match answered {
        Answered::Taken => (json(200, serde_json::json!({ "answered": true })), detail),
        Answered::Invalid(why) => (error(400, &why), String::new()),
        Answered::Unknown => (error(404, "no such dialog"), String::new()),
        Answered::Elsewhere => (
            refused(
                "answered",
                "the dialog is no longer up: answered at the terminal or by another device",
            ),
            "dialog refused: answered".into(),
        ),
        Answered::Gone => (
            refused("gone", "the dialog's hook is gone"),
            "dialog refused: gone".into(),
        ),
        Answered::Refused(refusal) => (
            super::input::refused(&refusal),
            format!("dialog refused: {}", refusal.code()),
        ),
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
