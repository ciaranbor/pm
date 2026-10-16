//! `pm harness hooks session-end <harness>`: the harness's session-end hook,
//! which reads the agent dead the moment its harness exits rather than once
//! the process is gone ([`running_agents`](crate::commands::running_agents)).
//! It stamps the harness process ([`runtime::mark_ended`]) when the payload
//! says the process is exiting ([`Harness::session_exited`]); the session
//! giving way to another in the same process is no end.
//!
//! The agent stays active, so a message still heals it. It must be quick —
//! Claude Code gives the event 1.5 s — so it reads the registry
//! only to skip an agent a delete removed, and leaves the tmux refresh to a
//! detached process. It prints nothing and always exits 0.

use std::io::Read;
use std::path::Path;

use crate::commands::hook_process::Caller;
use crate::error::Result;
use crate::harness::Harness;
use crate::messages;
use crate::state::paths;
use crate::state::runtime;

/// Run the hook. `on_end` gets the agent's unread message count once it is
/// stamped ended.
pub fn session_end(harness: Harness, on_end: impl FnOnce(u32)) -> i32 {
    if let Ok(Some(unread)) = session_end_inner(harness) {
        on_end(unread);
    }
    0
}

fn session_end_inner(harness: Harness) -> Result<Option<u32>> {
    let Some(agent) = std::env::var("PM_AGENT_NAME")
        .ok()
        .filter(|a| !a.is_empty())
    else {
        return Ok(None);
    };
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&input) else {
        return Ok(None);
    };
    let (project_root, scope) = paths::agent_scope()?;
    let pid = Caller::current().pid();
    if !end(&project_root, &scope, &agent, harness, &payload, pid)? {
        return Ok(None);
    }
    Ok(Some(messages::unread_count(
        &paths::messages_dir(&project_root),
        &scope,
        &agent,
    )))
}

/// Stamp `agent`'s harness process `pid` ended when `payload` says it is
/// exiting and the agent is still registered. Returns whether it did.
fn end(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    payload: &serde_json::Value,
    pid: u32,
) -> Result<bool> {
    if !harness.session_exited(payload) || !runtime::registered(project_root, scope, agent) {
        return Ok(false);
    }
    runtime::mark_ended(project_root, scope, agent, pid)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::agent::AgentRegistry;
    use crate::testing::agent_entry;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn only_an_exit_of_a_registered_agent_is_stamped() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut registry = AgentRegistry::default();
        registry.register("qa", agent_entry("qa"));
        registry.save(&paths::agents_dir(root), "login").unwrap();
        let stamped = |agent: &str, reason: &str| {
            let payload = json!({"hook_event_name": "SessionEnd", "reason": reason});
            end(root, "login", agent, Harness::ClaudeCode, &payload, 4242).unwrap()
        };

        assert!(!stamped("qa", "clear"));
        assert_eq!(runtime::read_ended(root, "login", "qa"), None);
        assert!(!stamped("deleted", "prompt_input_exit"));
        assert_eq!(runtime::read_ended(root, "login", "deleted"), None);
        assert!(stamped("qa", "prompt_input_exit"));
        assert_eq!(runtime::read_ended(root, "login", "qa"), Some(4242));
    }
}
