//! Registry and state records with the defaults most tests want; a test
//! overrides the fields it is about with struct-update syntax.

use std::path::Path;

use crate::harness::Harness;
use crate::state::agent::{AgentEntry, AgentType};
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::project::{AgentsConfig, ProjectConfig, ProjectEntry, ProjectInfo};

/// An active Claude Code agent in `window`, with no session or definition
/// alias.
pub fn agent_entry(window: &str) -> AgentEntry {
    AgentEntry {
        agent_type: AgentType::Agent,
        session_id: String::new(),
        window_name: window.to_string(),
        active: true,
        agent_definition: None,
        harness: Harness::ClaudeCode,
        spawned_at: None,
    }
}

/// A `wip` feature on `branch`, in a worktree of the same name, created now.
pub fn feature_state(branch: &str) -> FeatureState {
    let now = chrono::Utc::now();
    FeatureState {
        status: FeatureStatus::Wip,
        team: Default::default(),
        branch: branch.to_string(),
        worktree: branch.to_string(),
        base: String::new(),
        pr: String::new(),
        context: String::new(),
        workflow: None,
        created: now,
        last_active: now,
    }
}

/// A registry entry for a project at `root` on `main`, with no remotes.
pub fn project_entry(root: impl AsRef<Path>) -> ProjectEntry {
    ProjectEntry {
        root: root.as_ref().to_string_lossy().into_owned(),
        main_branch: "main".to_string(),
        repo_url: None,
        state_remote: None,
    }
}

/// Register the project at `root` as `name` in `registry`.
pub fn register_project(registry: &Path, name: &str, root: &Path) {
    project_entry(root).save(registry, name).unwrap();
}

/// A project config naming the project `name`, otherwise default.
pub fn project_config(name: &str) -> ProjectConfig {
    ProjectConfig {
        project: ProjectInfo {
            name: name.to_string(),
            max_features: None,
            fresh_base: None,
        },
        agents: AgentsConfig::default(),
        harness: Default::default(),
    }
}
