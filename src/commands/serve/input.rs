//! The write endpoints: text, an interrupt and keys for an agent, typed
//! into its pane ([`agent_input`]), and a dialog's answer
//! ([`super::dialog`]). Typed text is confirmed by finding it in the agent's
//! conversation; text queued mid-turn is answered at once, and the client
//! sees it arrive on its watch.

use std::time::{Duration, Instant};

use crate::commands::agent_input::{self, Delivery, KEYS, Refusal, Typed};
use crate::error::Result;
use crate::harness::Conversation;

use super::routes::{Reply, error, json};
use super::transcript::Agent;

/// The longest text a device may send, in bytes.
pub(super) const MAX_TEXT: usize = 128 * 1024;
/// The most keys one request presses.
const MAX_KEYS: usize = 32;
/// How long a prompt typed at the agent's prompt is looked for in its
/// conversation.
const CONFIRM_WITHIN: Duration = Duration::from_secs(5);

/// What a write did, for the request log: never the text itself.
pub(super) struct Written {
    pub reply: Reply,
    pub detail: String,
}

/// `POST …/{action}` for `agent`.
pub(super) fn post(
    agent: &Agent,
    action: &str,
    body: &str,
    tmux_server: Option<&str>,
) -> Result<Written> {
    let (root, scope, name) = (&agent.root, agent.scope.as_str(), agent.name.as_str());
    match action {
        "input" => {
            let text = match parse(body, "text").and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or("text is a string".to_string())
            }) {
                Ok(text) if text.trim().is_empty() => return Ok(bad("text is empty")),
                Ok(text) if text.len() > MAX_TEXT => {
                    return Ok(bad(&format!("text is over {MAX_TEXT} bytes")));
                }
                Ok(text) => text,
                Err(e) => return Ok(bad(&e)),
            };
            let detail = format!("text sha256:{}", agent_input::sha256(&text));
            let reply = match agent_input::send_text(root, scope, name, &text, tmux_server)? {
                Err(refusal) => refused(&refusal),
                Ok(Typed {
                    delivery: Delivery::Queued,
                    ..
                }) => delivered(Delivery::Queued, None),
                Ok(Typed {
                    delivery: Delivery::Sent,
                    after,
                }) => {
                    let confirmed = match (agent.conversation()?, after) {
                        (Some(c), Some(after)) => confirm(&c, &after, &text, CONFIRM_WITHIN)?,
                        _ => false,
                    };
                    delivered(Delivery::Sent, Some(confirmed))
                }
            };
            Ok(Written { reply, detail })
        }
        "interrupt" => {
            let reply = match agent_input::interrupt(root, scope, name, tmux_server)? {
                Ok(()) => json(200, serde_json::json!({})),
                Err(refusal) => refused(&refusal),
            };
            Ok(Written {
                reply,
                detail: "interrupt".into(),
            })
        }
        "dialog" => super::dialog::post(agent, body),
        "keys" => {
            let keys: Vec<String> = match parse(body, "keys").and_then(|v| {
                serde_json::from_value(v).map_err(|_| "keys is a list of key names".to_string())
            }) {
                Ok(keys) => keys,
                Err(e) => return Ok(bad(&e)),
            };
            let detail = format!("keys {}", keys.join(" "));
            if keys.is_empty() || keys.len() > MAX_KEYS {
                return Ok(Written {
                    reply: error(400, &format!("keys names 1 to {MAX_KEYS} keys")),
                    detail,
                });
            }
            if let Some(key) = keys.iter().find(|k| !KEYS.contains(&k.as_str())) {
                return Ok(Written {
                    reply: error(
                        400,
                        &format!("{key} is not a key a device may press: {}", KEYS.join(" ")),
                    ),
                    detail,
                });
            }
            let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
            let reply = match agent_input::send_keys(root, scope, name, &keys, tmux_server)? {
                Ok(()) => json(200, serde_json::json!({})),
                Err(refusal) => refused(&refusal),
            };
            Ok(Written { reply, detail })
        }
        _ => Ok(Written {
            reply: error(404, "no such endpoint"),
            detail: String::new(),
        }),
    }
}

/// The field `key` of the JSON object `body`.
fn parse(body: &str, key: &str) -> std::result::Result<serde_json::Value, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "the body is not JSON".to_string())?;
    value
        .get_mut(key)
        .map(serde_json::Value::take)
        .ok_or_else(|| format!("the body names no {key}"))
}

fn bad(message: &str) -> Written {
    Written {
        reply: error(400, message),
        detail: String::new(),
    }
}

fn refused(refusal: &Refusal) -> Reply {
    json(
        409,
        serde_json::json!({ "error": refusal.to_string(), "refused": refusal.code() }),
    )
}

fn delivered(delivery: Delivery, confirmed: Option<bool>) -> Reply {
    let mut body = serde_json::json!({ "delivery": delivery.as_str() });
    if let Some(confirmed) = confirmed {
        body["confirmed"] = confirmed.into();
    }
    json(200, body)
}

/// Whether the user's `text` shows up in `conversation` after cursor
/// `after` within `within`.
fn confirm(conversation: &Conversation, after: &str, text: &str, within: Duration) -> Result<bool> {
    let deadline = Instant::now() + within;
    let sha = agent_input::sha256(text);
    loop {
        if agent_input::said(conversation, after, &sha)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
