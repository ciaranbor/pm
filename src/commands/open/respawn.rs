//! Respawning a scope's active agents and choosing the window a restored
//! session lands on.

use std::path::Path;

use crate::commands::agent_spawn;
use crate::commands::launch_check::Launch;
use crate::error::Result;
use crate::state::agent::{AgentRegistry, AgentType};
use crate::tmux;

/// Respawn agents for a given scope.
///
/// Adds each agent respawned to `launched`; returns the spawn errors. A
/// session that had agents respawned lands on its first agent window.
pub(super) fn respawn_agents_for_scope(
    project_root: &Path,
    scope: &str,
    session_name: &str,
    agents_dir: &Path,
    tmux_server: Option<&str>,
    launched: &mut Vec<Launch>,
) -> Result<Vec<String>> {
    let spawn_result = agent_spawn::agent_spawn_all(project_root, scope, tmux_server)?;
    let spawned = spawn_result.spawned_count;
    launched.extend(spawn_result.launched().map(|agent| Launch {
        project_root: project_root.to_path_buf(),
        scope: scope.to_string(),
        agent: agent.to_string(),
    }));

    if spawned > 0 {
        let registry = AgentRegistry::load(agents_dir, scope)?;
        let first_agent = registry
            .agents
            .iter()
            .filter(|(_, e)| e.agent_type == AgentType::Agent)
            .find_map(|(_, e)| {
                tmux::find_window(tmux_server, session_name, &e.window_name)
                    .ok()
                    .flatten()
            });
        if let Some(target) = first_agent {
            let _ = tmux::select_window(tmux_server, &target);
        }
    }

    Ok(spawn_result
        .errors
        .iter()
        .map(ToString::to_string)
        .collect())
}
