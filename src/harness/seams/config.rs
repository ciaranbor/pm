//! Config seams: the harness's `[harness.<name>]` section and what it
//! accepts in an agent's `[agents.models]` and `[agents.permissions]` rows.

use std::path::Path;

use crate::harness::{Harness, opencode};
use crate::state::project::HarnessConfig;

impl Harness {
    /// Whether an agent on this harness is refused without an
    /// `[agents.models]` row.
    pub fn requires_model(self) -> bool {
        match self {
            Harness::ClaudeCode | Harness::Codex => false,
            Harness::OpenCode => true,
        }
    }

    /// What is wrong with an agent's `[agents.models]` and
    /// `[agents.permissions]` rows, for a harness that refuses a row it
    /// cannot take instead of passing it through.
    pub fn row_issues(self, model: Option<&str>, permission_mode: Option<&str>) -> Vec<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => Vec::new(),
            Harness::OpenCode => opencode::row_issues(model, permission_mode),
        }
    }

    /// What a spawn remarks on about an agent's `[agents.models]` row
    /// without refusing it.
    pub fn row_notes(self, config: &HarnessConfig, model: Option<&str>) -> Vec<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => Vec::new(),
            Harness::OpenCode => opencode::row_notes(&config.opencode, model),
        }
    }

    /// What is wrong with the harness's `[harness.<name>]` settings for
    /// agents started in `worktree`, where only the harness can tell. `rows`
    /// are the `[agents.models]` rows of the agents on this harness.
    pub fn config_issues(
        self,
        config: &HarnessConfig,
        worktree: &Path,
        rows: &[String],
    ) -> Vec<ConfigIssue> {
        match self {
            Harness::ClaudeCode | Harness::Codex => Vec::new(),
            Harness::OpenCode => opencode::config_issues(&config.opencode, worktree, rows),
        }
    }

    /// This harness's own `[harness.<name>]` section of `config`.
    pub fn config_section(self, config: &HarnessConfig) -> serde_json::Value {
        match self {
            Harness::ClaudeCode => serde_json::Value::Null,
            Harness::Codex => serde_json::to_value(&config.codex).unwrap_or_default(),
            Harness::OpenCode => serde_json::to_value(&config.opencode).unwrap_or_default(),
        }
    }
}

/// A finding about a harness's `[harness.<name>]` settings
/// ([`Harness::config_issues`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub kind: ConfigIssueKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigIssueKind {
    /// Settings the harness or pm refuses or drops.
    Invalid,
    /// A variable a setting names is unset in pm's environment; the agent's
    /// own environment may still set it.
    KeyUnset,
    /// A provider a model or definition needs that pm's agents cannot reach.
    ProviderUnreachable,
}
