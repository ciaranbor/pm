//! The **inbound channel**: where a process outside the agent's session
//! posts it a prompt without typing into its window, as the session
//! reported at its start
//! ([`Harness::inbound`](crate::harness::Harness::inbound)).
//! Every spawn forgets it until the new session starts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{agent_dir, agent_file};
use crate::error::Result;
use crate::fs_utils::write_atomic;

const INBOUND_FILE: &str = "inbound.json";

/// A session's inbound channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inbound {
    /// The unix socket the session listens on.
    pub socket: PathBuf,
    /// What tells the session a post is from one of its own, if it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The conversation to post to, for a socket that serves several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

/// Record `inbound` as the agent's current session's, or forget it (`None`).
pub fn write_inbound(
    project_root: &Path,
    scope: &str,
    agent: &str,
    inbound: Option<&Inbound>,
) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(INBOUND_FILE);
    match inbound {
        Some(inbound) => {
            write_atomic(&file, serde_json::to_string(inbound)?.as_bytes())?;
            let owner_only = std::os::unix::fs::PermissionsExt::from_mode(0o600);
            Ok(std::fs::set_permissions(&file, owner_only)?)
        }
        None => match std::fs::remove_file(file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        },
    }
}

/// The agent's current session's inbound channel, once it has started and
/// reported one.
pub fn read_inbound(project_root: &Path, scope: &str, agent: &str) -> Option<Inbound> {
    let text =
        std::fs::read_to_string(agent_file(project_root, scope, agent, INBOUND_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}
