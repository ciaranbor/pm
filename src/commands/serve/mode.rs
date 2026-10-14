//! An agent's permission mode: `GET …/mode` reads it with the modes a device
//! may pick, and `POST …/mode` sets it ([`agent_input::set_mode`]). Not
//! part of the snapshot: reading a mode may take a screen capture.

use crate::commands::agent_input;
use crate::error::Result;
use crate::harness::Harness;

use super::input::{Written, parse, refused};
use super::routes::{Reply, error, json};
use super::transcript::Agent;

/// The modes `harness` offers, as the API lists them.
fn modes(harness: Harness) -> serde_json::Value {
    harness
        .modes()
        .iter()
        .map(|(id, label)| serde_json::json!({ "id": id, "label": label }))
        .collect()
}

/// The harness of `agent` when pm switches its mode, else the reply saying
/// it doesn't.
fn switchable(agent: &Agent) -> Result<std::result::Result<Harness, Reply>> {
    Ok(match agent.harness()? {
        Some(harness) if !harness.modes().is_empty() => Ok(harness),
        _ => Err(error(404, "the agent's harness has no modes to switch")),
    })
}

/// `GET …/mode`.
pub(super) fn get(agent: &Agent, tmux_server: Option<&str>) -> Result<Reply> {
    let harness = match switchable(agent)? {
        Ok(harness) => harness,
        Err(reply) => return Ok(reply),
    };
    let mode =
        agent_input::current_mode(&agent.root, &agent.scope, &agent.name, harness, tmux_server)?;
    Ok(json(
        200,
        serde_json::json!({ "mode": mode, "modes": modes(harness) }),
    ))
}

/// `POST …/mode` with `{"mode": id}`.
pub(super) fn post(agent: &Agent, body: &str, tmux_server: Option<&str>) -> Result<Written> {
    let harness = match switchable(agent)? {
        Ok(harness) => harness,
        Err(reply) => {
            return Ok(Written {
                reply,
                detail: String::new(),
            });
        }
    };
    let mode = match parse(body, "mode").map(|v| v.as_str().map(str::to_string)) {
        Ok(Some(mode)) => mode,
        Ok(None) => return Ok(bad("mode is a string", String::new())),
        Err(e) => return Ok(bad(&e, String::new())),
    };
    let detail = format!("mode {mode}");
    if let Some(unknown) = agent_input::unknown_mode(harness, &mode) {
        return Ok(bad(&unknown, detail));
    }
    let reply =
        match agent_input::set_mode(&agent.root, &agent.scope, &agent.name, &mode, tmux_server)? {
            Ok(now) => json(200, serde_json::json!({ "mode": now })),
            Err(refusal) => refused(&refusal),
        };
    Ok(Written { reply, detail })
}

fn bad(message: &str, detail: String) -> Written {
    Written {
        reply: error(400, message),
        detail,
    }
}
