//! `pm msg send`: a queue that never spawns a new agent. A send errors on
//! an unregistered or inactive recipient, and heals an active one after
//! queuing — a dead window, or a harness that exited to the shell in its
//! pane, is respawned — and re-arms a live one no message would wake
//! ([`agent_rearm`](super::agent_rearm)). This holds for a recipient in
//! another scope or another project alike: the heal runs in the
//! recipient's project.
//!
//! The message records the sender's *home* — the project and scope whose
//! inbox its waiter watches — relative to the recipient, so `pm msg reply`
//! routes back there. A sender with no home (outside every project) can
//! send with `--project` but cannot be replied to, nor can one at a shell:
//! no waiter watches its inbox, and its home serves only the hint when the
//! recipient is missing.

mod hint;
mod state;
mod upstream;

pub use upstream::resolve_upstream;

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use chrono::Utc;

use crate::messages::{self, MessageMeta};
use crate::state::agent::AgentRegistry;
use crate::state::project::ProjectConfig;
use crate::state::{paths, scope};
use hint::agent_not_found_hint;

/// The project and scope whose inbox a sender's waiter watches: where a
/// reply to it is delivered.
#[derive(Debug, Clone, Copy)]
pub struct Home<'a> {
    pub root: &'a Path,
    pub scope: &'a str,
}

/// Who a message is from: a name, the scope it sends from, if any, and
/// whether it is at a shell rather than an agent.
#[derive(Debug, Clone, Copy)]
pub struct Sender<'a> {
    pub name: &'a str,
    pub home: Option<Home<'a>>,
    pub shell: bool,
}

/// Where the sender's home lies relative to the recipient's inbox.
enum Route {
    SameScope,
    OtherScope(String),
    OtherProject { scope: String, project: String },
    Outside,
}

impl Route {
    fn of(home: Option<Home<'_>>, target_root: &Path, target_scope: &str) -> Result<Self> {
        let Some(home) = home else {
            return Ok(Self::Outside);
        };
        if !paths::same_root(home.root, target_root) {
            let project = ProjectConfig::load(&paths::pm_dir(home.root))?.project.name;
            return Ok(Self::OtherProject {
                scope: home.scope.to_string(),
                project,
            });
        }
        Ok(if home.scope == target_scope {
            Self::SameScope
        } else {
            Self::OtherScope(home.scope.to_string())
        })
    }

    /// Whether the sender is outside the recipient's project.
    fn outside_target(&self) -> bool {
        matches!(self, Self::OtherProject { .. } | Self::Outside)
    }

    fn meta(&self, sender: Sender<'_>) -> MessageMeta {
        let from_outside = matches!(self, Self::Outside);
        let from_shell = sender.shell && !from_outside;
        let (sender_scope, sender_project) = match self {
            _ if from_shell => (None, None),
            Self::SameScope | Self::Outside => (None, None),
            Self::OtherScope(scope) => (Some(scope.clone()), None),
            Self::OtherProject { scope, project } => (Some(scope.clone()), Some(project.clone())),
        };
        MessageMeta {
            sender: sender.name.to_string(),
            timestamp: Utc::now(),
            sender_scope,
            sender_project,
            from_outside,
            from_shell,
        }
    }
}

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
    /// What went wrong after the message was queued.
    pub warnings: Vec<String>,
}

/// A dead recipient window a send respawned, for its caller to confirm
/// the launch of ([`launch_check`](super::launch_check)) before printing
/// `report`.
#[derive(Debug)]
pub struct Heal {
    pub root: PathBuf,
    pub scope: String,
    pub agent: String,
    pub report: String,
}

/// Fail as [`send_as`] would for a `recipient` in `feature` of the project
/// at `target_root` that no message can reach, without sending anything.
pub fn check_recipient(
    target_root: &Path,
    feature: &str,
    recipient: &str,
    sender: Sender<'_>,
) -> Result<()> {
    let route = Route::of(sender.home, target_root, feature)?;
    require_recipient(&route, target_root, feature, recipient, sender)
}

/// The recipient must be an active agent. Messaging never conjures a new
/// agent — agents are stood up by `pm feat new`/`feat adopt` (the whole
/// team) or `pm agent spawn`.
fn require_recipient(
    route: &Route,
    target_root: &Path,
    feature: &str,
    recipient: &str,
    sender: Sender<'_>,
) -> Result<()> {
    if recipient_is_active(target_root, feature, recipient)? {
        return Ok(());
    }
    let orchestrator = scope::orchestrator(target_root);
    Err(PmError::AgentNotFound(if route.outside_target() {
        let project = paths::project_name(target_root);
        let hint = hint::other_project_hint(recipient, feature, &orchestrator);
        format!(
            "No active agent called '{recipient}' exists in scope '{feature}' of project \
             '{project}'.{hint}"
        )
    } else {
        let sender_scope = sender.home.map_or(feature, |h| h.scope);
        let hint = agent_not_found_hint(recipient, sender_scope, feature, &orchestrator);
        format!("No active agent called '{recipient}' exists in scope '{feature}'.{hint}")
    }))
}

/// Send a message to `recipient` in scope `feature` of the project at
/// `target_root`.
///
/// Delivering to an inactive recipient is an error because nobody could
/// ever read the message; healing goes through `agent_spawn`, a no-op if
/// the harness runs.
pub fn send_as(
    target_root: &Path,
    feature: &str,
    recipient: &str,
    sender: Sender<'_>,
    body: &str,
    tmux_server: Option<&str>,
) -> Result<Sent> {
    let route = Route::of(sender.home, target_root, feature)?;

    require_recipient(&route, target_root, feature, recipient, sender)?;
    let found = state::read(target_root, feature, recipient, tmux_server);

    // Deliver the message first, then heal a dead window. This ensures the
    // message is durably queued in the inbox before the agent starts, so it
    // will be picked up on first read.
    let messages_dir = paths::messages_dir(target_root);
    let name = sender.name;
    let meta = route.meta(sender);
    let index = messages::send_meta(&messages_dir, feature, recipient, &meta, body)?;
    let in_project = match route {
        Route::OtherProject { .. } => format!(" in project '{}'", paths::project_name(target_root)),
        _ => String::new(),
    };
    let mut status = match &route {
        _ if meta.from_shell => format!(
            "Message {index:03} sent to '{recipient}@{feature}'{in_project} (from '{name}' at a \
             shell, not an agent: replies have nowhere to go)"
        ),
        Route::SameScope => format!("Message {index:03} sent to '{recipient}' (from '{name}')"),
        Route::OtherScope(home_scope) => format!(
            "Message {index:03} sent to '{recipient}@{feature}' (from '{name}@{home_scope}')"
        ),
        Route::OtherProject { scope, project } => format!(
            "Message {index:03} sent to '{recipient}@{feature}' in project '{}' (from \
             '{name}@{scope}' in project '{project}')",
            paths::project_name(target_root)
        ),
        Route::Outside => format!(
            "Message {index:03} sent to '{recipient}@{feature}' in project '{}' (from \
             '{name}', outside every project: replies have nowhere to go)",
            paths::project_name(target_root)
        ),
    };

    // The agent is active, but its tmux window may have died (crash,
    // accidental kill) or its harness exited. Call `agent_spawn` to heal it:
    // a no-op (`AlreadyActive`) if the harness runs, a respawn/resume if
    // not. Pass `None` for `agent_definition` so `agent_spawn` reads the
    // stored definition from the registry entry — preserving aliases. Only
    // a heal that happened is reported.
    //
    // The message is already queued, so a heal failure (e.g. the tmux
    // socket is unreachable from a sandbox) is not a delivery failure: a
    // warning, not an error, so the sender never thinks the message was lost.
    let mut heal = None;
    let mut warnings = Vec::new();
    match super::agent_spawn::agent_spawn(target_root, feature, recipient, None, None, tmux_server)
    {
        Ok((outcome, spawn_msg, _)) if outcome.is_new_window() => {
            heal = Some(Heal {
                root: target_root.to_path_buf(),
                scope: feature.to_string(),
                agent: recipient.to_string(),
                report: spawn_msg,
            });
        }
        Ok(_) => match super::agent_rearm::rearm(target_root, feature, recipient, tmux_server) {
            Ok(Some(waiting)) => {
                status = format!(
                    "{status}\n'{recipient}' was unarmed ({}): re-armed, it reads this now",
                    waiting.describe()
                );
            }
            Ok(None) => {
                if let Some(found) = &found {
                    status = format!("{status}\n{}", state::describe(recipient, found));
                }
            }
            Err(e) => warnings.push(format!(
                "message {index:03} is queued for '{recipient}@{feature}', but it could not be \
                 re-armed: {e}"
            )),
        },
        Err(e) => warnings.push(format!(
            "message {index:03} is queued for '{recipient}@{feature}', but its tmux window could \
             not be respawned: {e}\n  It will be read if and when that agent next runs."
        )),
    }

    Ok(Sent {
        status,
        heal,
        warnings,
    })
}

#[cfg(test)]
pub(crate) mod tests;
