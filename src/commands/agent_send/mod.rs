//! `pm msg send`: a queue that never spawns a new agent. Same-scope and
//! cross-scope sends (`--scope`/`--upstream`, same project) error on an
//! unregistered or inactive recipient, and heal an active one after
//! queuing — a dead window, or a harness that exited to the shell in its
//! pane, is respawned — and re-arm a live one no message would wake
//! ([`agent_rearm`](super::agent_rearm)); a cross-project send only queues,
//! since the target agent lives in a project this one can't spawn in.

mod cross_project;
mod hint;
mod upstream;

pub use cross_project::{CrossProjectSendParams, agent_send_cross_project};
pub use upstream::resolve_upstream;

use std::path::Path;

use crate::error::{PmError, Result};
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use hint::agent_not_found_hint;

/// Whether the recipient agent is currently flagged active — the only
/// thing a send needs to know, since it never resurrects a stopped agent;
/// a dead window of an active one is healed by `agent_spawn` after the
/// message is queued.
fn recipient_is_active(project_root: &Path, feature: &str, agent_name: &str) -> Result<bool> {
    let agents_dir = paths::agents_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;
    Ok(registry.get(agent_name).map(|e| e.active).unwrap_or(false))
}

/// What a send did.
#[derive(Debug)]
pub struct Sent {
    /// The lines saying what was sent, and any re-arm.
    pub status: String,
    pub heal: Option<Heal>,
}

/// A dead recipient window a send respawned, for its caller to confirm
/// the launch of ([`launch_check`](super::launch_check)) before printing
/// `report`.
#[derive(Debug)]
pub struct Heal {
    pub scope: String,
    pub agent: String,
    pub report: String,
}

/// Send a message to an agent's inbox.
///
/// Delivering to an inactive recipient is an error because nobody could
/// ever read the message; healing goes through `agent_spawn`, a no-op if
/// the harness runs.
///
/// `target_scope` is the scope (feature or "main") the message is delivered
/// to. When `None`, defaults to `sender_scope` (same-scope message).
///
/// `sender_scope` is the scope the sender is currently in, recorded in
/// message metadata so the recipient knows where the message came from.
pub fn agent_send(
    project_root: &Path,
    sender_scope: &str,
    target_scope: Option<&str>,
    recipient: &str,
    sender: &str,
    body: &str,
    tmux_server: Option<&str>,
) -> Result<Sent> {
    let feature = target_scope.unwrap_or(sender_scope);

    // The recipient must be an active agent. Messaging never conjures a new
    // agent — agents are stood up by `pm feat new`/`feat adopt` (the whole
    // team) or `pm agent spawn`.
    if !recipient_is_active(project_root, feature, recipient)? {
        let hint = agent_not_found_hint(recipient, sender_scope, feature);
        return Err(PmError::AgentNotFound(format!(
            "No active agent called '{recipient}' exists in scope '{feature}'.{hint}"
        )));
    }

    // Deliver the message first, then heal a dead window. This ensures the
    // message is durably queued in the inbox before the agent starts, so it
    // will be picked up on first read.
    let messages_dir = paths::messages_dir(project_root);
    let is_cross_scope = target_scope.is_some() && target_scope != Some(sender_scope);
    let index = messages::send_with_scope(
        &messages_dir,
        feature,
        recipient,
        sender,
        body,
        if is_cross_scope {
            Some(sender_scope)
        } else {
            None
        },
    )?;
    let mut status = if is_cross_scope {
        format!(
            "Message {index:03} sent to '{recipient}@{feature}' (from '{sender}@{sender_scope}')"
        )
    } else {
        format!("Message {index:03} sent to '{recipient}' (from '{sender}')")
    };

    // The agent is active, but its tmux window may have died (crash,
    // accidental kill) or its harness exited. Call `agent_spawn` to heal it:
    // a no-op (`AlreadyActive`) if the harness runs, a respawn/resume if
    // not. Pass `None` for `agent_definition` so `agent_spawn` reads the
    // stored definition from the registry entry — preserving aliases. Only
    // report a heal when one actually happened, keeping the common-case
    // output byte-identical.
    //
    // The message is already queued, so a heal failure (e.g. the tmux
    // socket is unreachable from a sandbox) is not a delivery failure: warn
    // and exit 0 rather than make the sender think the message was lost.
    let mut heal = None;
    match super::agent_spawn::agent_spawn(project_root, feature, recipient, None, None, tmux_server)
    {
        Ok((outcome, spawn_msg, _)) if outcome.is_new_window() => {
            heal = Some(Heal {
                scope: feature.to_string(),
                agent: recipient.to_string(),
                report: spawn_msg,
            });
        }
        Ok(_) => match super::agent_rearm::rearm(project_root, feature, recipient, tmux_server) {
            Ok(Some(waiting)) => {
                status = format!("{status}\nRe-armed '{recipient}' ({})", waiting.describe());
            }
            Ok(None) => {}
            Err(e) => eprintln!(
                "warning: message {index:03} is queued for '{recipient}@{feature}', but it could \
                 not be re-armed: {e}"
            ),
        },
        Err(e) => eprintln!(
            "warning: message {index:03} is queued for '{recipient}@{feature}', but its tmux \
             window could not be respawned: {e}\n  It will be read if and when that agent next runs."
        ),
    }

    Ok(Sent { status, heal })
}

#[cfg(test)]
mod tests;
