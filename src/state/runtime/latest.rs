//! The **latest reply**: the agent's last reply as the status views show it
//! ([`attention`](crate::commands::attention)), kept with the version of
//! the conversation it was read from, so a view refreshed while the
//! conversation is unchanged reads this file instead of the transcript.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{agent_dir, agent_file};
use crate::error::Result;
use crate::fs_utils::write_atomic;

const LATEST_FILE: &str = "latest.json";

/// An agent's last reply, on one line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Latest {
    pub text: String,
    /// When it was written, where the harness records it.
    pub at: Option<DateTime<Utc>>,
}

/// What [`write_latest`] recorded: the reply read at `version` of the
/// conversation, `None` when that read found none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestRecord {
    pub version: String,
    pub latest: Option<Latest>,
}

pub fn write_latest(
    project_root: &Path,
    scope: &str,
    agent: &str,
    record: &LatestRecord,
) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(LATEST_FILE);
    write_atomic(&file, serde_json::to_string(record)?.as_bytes())
}

pub fn read_latest(project_root: &Path, scope: &str, agent: &str) -> Option<LatestRecord> {
    let text = std::fs::read_to_string(agent_file(project_root, scope, agent, LATEST_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}
