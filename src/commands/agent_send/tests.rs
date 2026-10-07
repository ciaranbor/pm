//! `agent_send` end to end: delivery, the heal of a dead or exited
//! recipient, and the scope recorded on a cross-scope send.

mod heal;
mod recipient;
mod scope;

use super::*;
use crate::state::agent::{AgentEntry, AgentType};
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::project::{ProjectConfig, ProjectInfo};
use crate::testing::TestServer;
use crate::tmux;
use chrono::Utc;
use std::path::PathBuf;
use tempfile::tempdir;

/// Write `.pm/features/` and a project config named `project_name`.
fn setup_project_files(dir: &Path, project_name: &str) -> PathBuf {
    let root = dir.to_path_buf();
    let pm_dir = root.join(".pm");
    std::fs::create_dir_all(pm_dir.join("features")).unwrap();

    let config = ProjectConfig {
        project: ProjectInfo {
            name: project_name.to_string(),
            max_features: None,
        },
        agents: Default::default(),
        harness: Default::default(),
    };
    config.save(&pm_dir).unwrap();
    root
}

/// Set up a project with a tmux session for the feature.
fn setup_project_with_tmux(dir: &Path, server: &TestServer) -> (PathBuf, String, String) {
    let project_name = server.scope("proj");
    let feature_name = "login";
    let root = setup_project_files(dir, &project_name);
    let pm_dir = root.join(".pm");

    let now = Utc::now();
    let state = FeatureState {
        status: FeatureStatus::Wip,
        branch: feature_name.to_string(),
        worktree: feature_name.to_string(),
        base: String::new(),
        pr: String::new(),
        context: String::new(),
        workflow: None,
        created: now,
        last_active: now,
        progress: Default::default(),
        blocked_reason: None,
        blocked_by: None,
    };
    state.save(&pm_dir.join("features"), feature_name).unwrap();

    let worktree = root.join(feature_name);
    std::fs::create_dir_all(&worktree).unwrap();

    let session_name = tmux::session_name(&project_name, feature_name);
    tmux::create_session(server.name(), &session_name, &worktree).unwrap();

    (root, session_name, feature_name.to_string())
}

/// Create an agent definition in the main worktree (where `pm agents install-project` writes).
fn create_agent_definition(root: &Path, agent_name: &str) {
    let agent_def = paths::main_worktree(root)
        .join(".agents/agents")
        .join(format!("{agent_name}.md"));
    std::fs::create_dir_all(agent_def.parent().unwrap()).unwrap();
    std::fs::write(&agent_def, "# agent stub").unwrap();
}
