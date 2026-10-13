//! `agent_send` end to end: delivery, the heal of a dead or exited
//! recipient, and the scope recorded on a cross-scope send.

mod heal;
mod other_project;
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
            fresh_base: None,
        },
        agents: Default::default(),
        harness: Default::default(),
    };
    config.save(&pm_dir).unwrap();
    root
}

/// Set up a project with a tmux session for the feature.
pub(crate) fn setup_project_with_tmux(
    dir: &Path,
    server: &TestServer,
) -> (PathBuf, String, String) {
    let (root, session_name) = setup_named_project(dir, server, "proj", "login");
    (root, session_name, "login".to_string())
}

/// A project named `server.scope(name)` at `dir`, with a main worktree and
/// a worktree and tmux session for `scope_name`.
fn setup_named_project(
    dir: &Path,
    server: &TestServer,
    name: &str,
    scope_name: &str,
) -> (PathBuf, String) {
    let project_name = server.scope(name);
    let root = setup_project_files(dir, &project_name);
    std::fs::create_dir_all(paths::main_worktree(&root)).unwrap();

    if scope_name != crate::state::scope::MAIN {
        let now = Utc::now();
        let state = FeatureState {
            status: FeatureStatus::Wip,
            branch: scope_name.to_string(),
            worktree: scope_name.to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: now,
            last_active: now,
            team: Default::default(),
        };
        state.save(&root.join(".pm/features"), scope_name).unwrap();
    }

    let worktree = root.join(scope_name);
    std::fs::create_dir_all(&worktree).unwrap();
    let session_name = tmux::session_name(&project_name, scope_name);
    tmux::create_session(server.name(), &session_name, &worktree).unwrap();
    (root, session_name)
}

/// Create an agent definition in the main worktree (where `pm agents install-project` writes).
fn create_agent_definition(root: &Path, agent_name: &str) {
    let agent_def = paths::main_worktree(root)
        .join(".agents/agents")
        .join(format!("{agent_name}.md"));
    std::fs::create_dir_all(agent_def.parent().unwrap()).unwrap();
    std::fs::write(&agent_def, "# agent stub").unwrap();
}

/// Send a message from `sender` to an agent in the sender's own project:
/// [`send_as`] with a home of `(project_root, sender_scope)`. `target_scope`
/// defaults to `sender_scope`.
pub(crate) fn agent_send(
    project_root: &Path,
    sender_scope: &str,
    target_scope: Option<&str>,
    recipient: &str,
    sender: &str,
    body: &str,
    tmux_server: Option<&str>,
) -> Result<Sent> {
    let sender = Sender {
        name: sender,
        home: Some(Home {
            root: project_root,
            scope: sender_scope,
        }),
        shell: false,
    };
    send_as(
        project_root,
        target_scope.unwrap_or(sender_scope),
        recipient,
        sender,
        body,
        tmux_server,
    )
}
