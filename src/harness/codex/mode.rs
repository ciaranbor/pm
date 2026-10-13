//! Codex's collaboration mode, Plan or Default — the one ⇧Tab toggles in
//! the TUI — read and set through the agent's own app-server (verified on
//! 0.162.1). Its permission presets are left alone: a sandboxed agent
//! can't wake others, and an approval prompt stalls an unwatched one. Once
//! the user picks a `/permissions` preset in the TUI, its later turns
//! re-send that preset's sandbox, whatever pm set.
//!
//! `thread/resume` with `excludeTurns`, from a connection of pm's own on a
//! thread the TUI has loaded, answers with the thread's `collaborationMode`
//! and `model` and leaves the TUI working; `thread/read` carries neither,
//! and the rollout records the mode only after a turn. The resume
//! subscribes the connection to the thread, so each connection is
//! short-lived. The screen is no use: Plan shows `Plan mode` in the footer,
//! Default nothing.
//!
//! `thread/settings/update` (experimental) with `collaborationMode` sets
//! it, needing no turn, for every later turn; the attached TUI shows the
//! change at once and keeps it on its own turns. The value is what the TUI
//! sends on ⇧Tab (`tui/src/collaboration_modes.rs`, tag `rust-v0.162.1`):
//! the thread's current settings with the server's preset for the mode
//! (`collaborationMode/list`) laid over them — its model and reasoning
//! effort where it names one — and the developer instructions cleared, so
//! the server supplies the mode's own.

use std::time::Duration;

use serde_json::{Value, json};

use super::app_server::Client;
use crate::error::{PmError, Result};
use crate::state::runtime::Inbound;

/// The modes a device may pick, by the ids codex uses, with the TUI's
/// names for them.
pub(in crate::harness) const MODES: &[(&str, &str)] = &[("default", "Default"), ("plan", "Plan")];

/// How long each exchange with the server may take.
const LIMIT: Duration = Duration::from_secs(3);

/// The mode of the session on `inbound`; `None` when its server reports
/// none.
pub(in crate::harness) fn current(inbound: &Inbound) -> Result<Option<String>> {
    let (mut client, thread) = connect(inbound)?;
    Ok(mode_of(&resume(&mut client, &thread)?))
}

/// Set the session on `inbound` to `mode`, and return the mode its server
/// then reports.
pub(in crate::harness) fn set(inbound: &Inbound, mode: &str) -> Result<Option<String>> {
    let (mut client, thread) = connect(inbound)?;
    let resumed = resume(&mut client, &thread)?;
    let presets = client.call("collaborationMode/list", json!({}))?;
    let preset = presets
        .get("data")
        .and_then(Value::as_array)
        .and_then(|data| data.iter().find(|p| p["mode"] == mode))
        .cloned()
        .unwrap_or(Value::Null);
    let collaboration_mode = applied(&resumed, mode, &preset);
    client.call(
        "thread/settings/update",
        json!({"threadId": thread, "collaborationMode": collaboration_mode}),
    )?;
    Ok(mode_of(&resume(&mut client, &thread)?))
}

fn connect(inbound: &Inbound) -> Result<(Client, String)> {
    let thread = inbound
        .thread
        .clone()
        .ok_or_else(|| PmError::Agent("its codex session reported no thread".to_string()))?;
    let socket = inbound.socket().ok_or_else(|| {
        PmError::Agent("its codex session reported no app-server socket".to_string())
    })?;
    Ok((Client::connect(socket, LIMIT)?, thread))
}

fn resume(client: &mut Client, thread: &str) -> Result<Value> {
    client.call(
        "thread/resume",
        json!({"threadId": thread, "excludeTurns": true}),
    )
}

fn mode_of(resumed: &Value) -> Option<String> {
    resumed["collaborationMode"]["mode"]
        .as_str()
        .map(str::to_string)
}

/// The `collaborationMode` that puts the thread `resumed` describes in
/// `mode`, its `preset` from `collaborationMode/list` (or null) applied.
fn applied(resumed: &Value, mode: &str, preset: &Value) -> Value {
    let settings = &resumed["collaborationMode"]["settings"];
    let current = |field: &str, fallback: &Value| match &settings[field] {
        Value::Null => fallback.clone(),
        value => value.clone(),
    };
    let model = match &preset["model"] {
        Value::String(model) => json!(model),
        _ => current("model", &resumed["model"]),
    };
    let effort = match &preset["reasoning_effort"] {
        Value::String(effort) => json!(effort),
        _ => current("reasoning_effort", &resumed["reasoningEffort"]),
    };
    json!({
        "mode": mode,
        "settings": {
            "model": model,
            "reasoning_effort": effort,
            "developer_instructions": null,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::runtime::Endpoint;
    use crate::testing::FakeAppServer;

    fn inbound(server: &FakeAppServer) -> Inbound {
        Inbound {
            endpoint: Endpoint::Socket(server.path.clone()),
            token: None,
            thread: Some("t1".into()),
        }
    }

    fn skip_handshake(server: &FakeAppServer) {
        assert_eq!(server.request()["method"], "initialize");
        assert_eq!(server.request()["method"], "initialized");
    }

    #[test]
    fn the_mode_is_read_from_a_resume_without_turns() {
        let dir = tempfile::tempdir().unwrap();
        let server = FakeAppServer::bind(dir.path());

        assert_eq!(
            current(&inbound(&server)).unwrap().as_deref(),
            Some("default")
        );

        skip_handshake(&server);
        let resume = server.request();
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(
            resume["params"],
            json!({"threadId": "t1", "excludeTurns": true})
        );
    }

    #[test]
    fn plan_is_set_with_the_threads_model_and_the_presets_effort() {
        let dir = tempfile::tempdir().unwrap();
        let server = FakeAppServer::bind(dir.path());

        assert_eq!(
            set(&inbound(&server), "plan").unwrap().as_deref(),
            Some("plan")
        );

        skip_handshake(&server);
        assert_eq!(server.request()["method"], "thread/resume");
        assert_eq!(server.request()["method"], "collaborationMode/list");
        let update = server.request();
        assert_eq!(update["method"], "thread/settings/update");
        assert_eq!(
            update["params"],
            json!({"threadId": "t1", "collaborationMode": {
                "mode": "plan",
                "settings": {
                    "model": "gpt-5.5-codex",
                    "reasoning_effort": "medium",
                    "developer_instructions": null,
                },
            }})
        );
    }

    #[test]
    fn a_preset_naming_no_effort_keeps_the_threads() {
        let resumed = json!({
            "model": "gpt-5.5-codex",
            "reasoningEffort": "high",
            "collaborationMode": {"mode": "plan", "settings": {
                "model": "gpt-5.5-codex", "reasoning_effort": "medium",
                "developer_instructions": "plan first",
            }},
        });
        let preset = json!({"name": "Default", "mode": "default", "model": null,
            "reasoning_effort": null});
        assert_eq!(
            applied(&resumed, "default", &preset)["settings"],
            json!({"model": "gpt-5.5-codex", "reasoning_effort": "medium",
                "developer_instructions": null})
        );

        let older = json!({"model": "gpt-5.5-codex", "reasoningEffort": "high"});
        assert_eq!(
            applied(&older, "default", &Value::Null)["settings"],
            json!({"model": "gpt-5.5-codex", "reasoning_effort": "high",
                "developer_instructions": null})
        );
    }
}
