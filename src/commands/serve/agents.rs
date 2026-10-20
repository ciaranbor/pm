//! An agent's own lifecycle from a device: the definitions a spawn can
//! name, and spawn, stop, delete and fork through the handlers `pm agent`
//! runs. Spawning a registered name does what `pm agent spawn` does: a
//! stopped or dead agent is resumed or healed, an active one only gets the
//! context. A refusal, which changed nothing, reaches the device in the
//! CLI's words with a `refused` code, except a way out only a terminal
//! offers, which is worded for the device.
//!
//! Stop and delete end the agent's window last, after its state is
//! written, so a `pm serve` run by hand in that window ends with it,
//! before it replies.

use std::path::Path;

use crate::commands::{agent_delete, agent_fork, agent_input, agent_spawn, launch_check};
use crate::error::{PmError, Result};
use crate::state::project::{GlobalConfig, ProjectConfig, resolve_agent_settings};
use crate::state::{paths, workflow};

use super::Config;
use super::input::{MAX_TEXT, Written};
use super::lifecycle::finish;
use super::routes::{Reply, error, json};
use super::transcript::Agent;

/// `GET projects/{project}/definitions` for the project at `root`: each
/// definition with the harness and model a spawn of it would launch now, or
/// the `error` config gives for it instead.
pub(super) fn definitions(root: &Path) -> Result<Reply> {
    let config = ProjectConfig::load(&paths::pm_dir(root))?;
    let global = GlobalConfig::load_or_default();
    let home = paths::home_dir().ok();
    let listed = workflow::list_definitions(root, home.as_deref())?
        .into_iter()
        .map(|d| {
            let mut entry = serde_json::json!({
                "name": d.name,
                "description": d.description,
                "harness": null,
                "model": null,
            });
            match resolve_agent_settings(&config.agents, &global.agents, &d.name) {
                Ok(settings) => {
                    entry["harness"] = settings.harness.to_string().into();
                    entry["model"] = settings.model.into();
                }
                Err(e) => entry["error"] = e.to_string().into(),
            }
            entry
        })
        .collect::<Vec<_>>();
    Ok(json(200, serde_json::json!({ "definitions": listed })))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnBody {
    definition: Option<String>,
    context: Option<String>,
}

/// `POST agents/{project}/{scope}/{agent}/spawn`, `{"definition",
/// "context"}` both optional: `pm agent spawn <agent> [--agent definition]
/// [--context context]` in `scope` of the project at `root`.
pub(super) fn spawn(
    config: &Config,
    root: &Path,
    scope: &str,
    name: &str,
    body: &str,
) -> Result<Written> {
    let body = if body.trim().is_empty() { "{}" } else { body };
    let Ok(SpawnBody {
        definition,
        context,
    }) = serde_json::from_str(body)
    else {
        return Ok(bad(
            "the body is {\"definition\": name, \"context\": text}, each optional, or empty",
        ));
    };
    let context = context.filter(|c| !c.trim().is_empty());
    if context.as_ref().is_some_and(|c| c.len() > MAX_TEXT) {
        return Ok(bad(&format!("context is longer than {MAX_TEXT} bytes")));
    }
    let mut detail = format!("spawn {name}");
    if let Some(definition) = &definition {
        detail.push_str(&format!(" --agent {definition}"));
    }
    if let Some(context) = &context {
        detail.push_str(&format!(" context sha256:{}", agent_input::sha256(context)));
    }
    let server = config.tmux_server.as_deref();
    let done = agent_spawn::agent_spawn(
        root,
        scope,
        name,
        definition.as_deref(),
        context.as_deref(),
        server,
    )
    .and_then(|(outcome, line, _)| {
        if outcome.is_new_window() {
            launch_check::check(root, scope, name, server)?;
        }
        let outcome = match outcome {
            agent_spawn::SpawnOutcome::Spawned => "spawned",
            agent_spawn::SpawnOutcome::Resumed => "resumed",
            agent_spawn::SpawnOutcome::AlreadyActive => "already-active",
        };
        Ok(serde_json::json!({ "spawned": line, "outcome": outcome }))
    });
    finish(config, detail, done, |e| refusal(scope, e))
}

/// `POST agents/{project}/{scope}/{agent}/{stop|delete}`.
pub(super) fn end(config: &Config, agent: &Agent, action: &str) -> Result<Written> {
    let server = config.tmux_server.as_deref();
    let (root, scope, name) = (&agent.root, agent.scope.as_str(), agent.name.as_str());
    let done = match action {
        "stop" => crate::commands::agent_stop::agent_stop(root, scope, name, server)
            .map(|line| serde_json::json!({ "stopped": line })),
        _ => agent_delete::agent_delete(root, scope, name, server)
            .map(|line| serde_json::json!({ "deleted": line })),
    };
    finish(config, format!("{action} {name}"), done, |e| {
        refusal(scope, e)
    })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ForkBody {
    name: String,
}

/// `POST agents/{project}/{scope}/{agent}/fork`, `{"name"}`: `pm agent fork
/// <agent> <name>`.
pub(super) fn fork(config: &Config, agent: &Agent, body: &str) -> Result<Written> {
    let Ok(ForkBody { name }) = serde_json::from_str(body) else {
        return Ok(bad("the body is {\"name\": the new agent's name}"));
    };
    let server = config.tmux_server.as_deref();
    let (root, scope) = (&agent.root, agent.scope.as_str());
    let done = agent_fork::agent_fork(root, scope, &agent.name, &name, server).and_then(|line| {
        launch_check::check(root, scope, &name, server)?;
        Ok(serde_json::json!({ "forked": line }))
    });
    let detail = format!("fork {} as {name}", agent.name);
    finish(config, detail, done, |e| refusal(scope, e))
}

fn bad(message: &str) -> Written {
    Written {
        reply: error(400, message),
        detail: String::new(),
    }
}

/// The refusals of an agent's spawn, stop, delete and fork in `scope`.
fn refusal(scope: &str, e: &PmError) -> Option<(&'static str, String)> {
    Some(match e {
        PmError::SessionNotRunning { .. } => (
            "no-session",
            format!(
                "the session of '{scope}' is not running (the project is closed, or its last \
                 agent was stopped): open the project, then retry"
            ),
        ),
        PmError::AgentDefinitionMissing { .. }
        | PmError::UnmigratedVanillaAgent
        | PmError::BundledDisabled { .. } => ("definition", e.to_string()),
        PmError::Unforkable(why) => ("fork", why.clone()),
        _ => return None,
    })
}
