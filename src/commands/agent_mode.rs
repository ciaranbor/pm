//! `pm agent mode`: an agent's permission mode, shown or switched the way
//! `pm serve`'s mode endpoint does it ([`agent_input::set_mode`]).

use std::fmt::Write;
use std::path::Path;

use crate::commands::agent_input;
use crate::error::{PmError, Result};
use crate::state::agent::AgentRegistry;
use crate::state::paths;

/// Switch agent `name` of `scope` to `mode`, or with none, show its mode
/// and the modes it may be switched to.
pub fn run(
    project_root: &Path,
    scope: &str,
    name: &str,
    mode: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<String> {
    let registry = AgentRegistry::load(&paths::agents_dir(project_root), scope)?;
    let Some(entry) = registry.get(name) else {
        return Err(PmError::Agent(format!(
            "no agent '{name}' in scope '{scope}'"
        )));
    };
    let harness = entry.harness;
    let modes = harness.modes();
    if modes.is_empty() {
        return Err(PmError::Agent(format!(
            "pm doesn't switch the mode of {} agents",
            harness.as_str()
        )));
    }
    let Some(mode) = mode else {
        let now = agent_input::current_mode(project_root, scope, name, harness, tmux_server)?;
        let mut out = format!("{}\n", now.as_deref().unwrap_or("unknown"));
        for (id, label) in modes {
            let mark = if now.as_deref() == Some(*id) {
                '*'
            } else {
                ' '
            };
            writeln!(out, "{mark} {id:<20} {label}").ok();
        }
        return Ok(out);
    };
    if let Some(unknown) = agent_input::unknown_mode(harness, mode) {
        return Err(PmError::Agent(format!("'{name}': {unknown}")));
    }
    match agent_input::set_mode(project_root, scope, name, mode, tmux_server)? {
        Ok(now) => Ok(format!("'{name}' is in {now}\n")),
        Err(refusal) => Err(PmError::Agent(format!("'{name}': {refusal}"))),
    }
}
