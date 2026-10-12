//! The **denial record**: the agent's latest tool call its harness's
//! permission classifier refused
//! ([`Harness::denial`](crate::harness::Harness::denial)).
//! The turn goes on, so it is no waiting marker, which the turn's end
//! replaces; it stands until the agent is prompted or spawned. A later tool
//! call can't clear it: the model's other calls in the same message finish
//! after the refusal is reported.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{agent_dir, agent_file};
use crate::error::Result;
use crate::fs_utils::write_atomic;

const DENIAL_FILE: &str = "denied.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Denial {
    /// The tool and what it acts on.
    pub detail: String,
    /// Why the classifier refused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub since: DateTime<Utc>,
}

impl Denial {
    pub fn describe(&self) -> String {
        match &self.reason {
            Some(reason) => format!("{} {reason}", self.detail),
            None => self.detail.clone(),
        }
    }
}

pub fn write_denial(project_root: &Path, scope: &str, agent: &str, denial: &Denial) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(DENIAL_FILE);
    write_atomic(&file, serde_json::to_string(denial)?.as_bytes())
}

pub fn read_denial(project_root: &Path, scope: &str, agent: &str) -> Option<Denial> {
    let text = std::fs::read_to_string(agent_file(project_root, scope, agent, DENIAL_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn clear_denial(project_root: &Path, scope: &str, agent: &str) -> Result<()> {
    match std::fs::remove_file(agent_file(project_root, scope, agent, DENIAL_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}
