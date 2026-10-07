//! The write endpoints: text, an interrupt, keys and text typed as keys
//! for an agent, all reaching its pane ([`agent_input`]), and a dialog's answer
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
/// The longest text `type` takes, in bytes.
const MAX_TYPED: usize = 4 * 1024;
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
            let text = match text_field(body, MAX_TEXT) {
                Ok(text) if text.trim().is_empty() => return Ok(bad("text is empty")),
                Ok(text) => text,
                Err(bad) => return Ok(bad),
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
        "dialog" => super::dialog::post(agent, body, tmux_server),
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
            if let Some(key) = keys.iter().find(|k| !agent_input::pressable(k)) {
                return Ok(Written {
                    reply: error(
                        400,
                        &format!(
                            "{key} is not a key a device may press: {}, or C- with a letter or arrow",
                            KEYS.join(" ")
                        ),
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
        "type" => {
            let text = match text_field(body, MAX_TYPED) {
                Ok(text) if text.is_empty() => return Ok(bad("text is empty")),
                Ok(text) if text.chars().any(char::is_control) => {
                    return Ok(bad("text holds a control character; press keys for those"));
                }
                Ok(text) => text,
                Err(bad) => return Ok(bad),
            };
            let reply = match agent_input::type_text(root, scope, name, &text, tmux_server)? {
                Ok(()) => json(200, serde_json::json!({})),
                Err(refusal) => refused(&refusal),
            };
            Ok(Written {
                reply,
                detail: format!("type sha256:{}", crate::hash::sha256_hex(text.as_bytes())),
            })
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

/// The string `text` of the JSON object `body`, of at most `max` bytes.
fn text_field(body: &str, max: usize) -> std::result::Result<String, Written> {
    let text = parse(body, "text").map_err(|e| bad(&e))?;
    let text = text.as_str().ok_or_else(|| bad("text is a string"))?;
    if text.len() > max {
        return Err(bad(&format!("text is over {max} bytes")));
    }
    Ok(text.to_string())
}

fn bad(message: &str) -> Written {
    Written {
        reply: error(400, message),
        detail: String::new(),
    }
}

pub(super) fn refused(refusal: &Refusal) -> Reply {
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
