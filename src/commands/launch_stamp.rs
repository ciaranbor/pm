//! The launch stamp: a hash of what a spawn launched an agent with, written
//! to its runtime dir ([`runtime::write_launch_stamp`]) by every spawn of a
//! named agent. An agent is stale when the stamp a spawn would write now
//! differs from its own, or it has none: it runs on a definition, prompt,
//! flags or never-idle loop it would not be launched with today, which a
//! running session does not pick up. `pm upgrade` restarts the idle ones
//! ([`super::agent_restart_all`]); `pm doctor` reports them.
//!
//! The spawn and the check compute the stamp through the one resolution
//! (`resolve_launch`) and the one hash (`stamp`): if they drifted, every
//! agent would read stale and every upgrade would restart every agent.
//!
//! The composed prompt is hashed by its text, not its path, which a notice
//! board moves. Only the agent's own `[harness.<name>]` section counts, so
//! an edit to another harness's restarts nothing. The command line is built
//! without what differs between spawns of one agent (its prompt, session
//! and prompt file). pm's never-idle loop counts only for a harness that
//! reads it at launch alone ([`hooks_reload_live`]). Skills are left out:
//! harnesses load them on demand. [`LAUNCH_EPOCH`] covers the rest.
//!
//! [`hooks_reload_live`]: crate::harness::Harness::hooks_reload_live

use std::path::Path;

use serde_json::json;

use crate::error::Result;
use crate::harness::{PreLaunch, SpawnSpec};
use crate::state::agent::AgentEntry;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig};
use crate::state::runtime;
use crate::state::workflow;

use super::agent_spawn::{LaunchConfig, definition_flag, resolve_launch};
use super::hooks_install;

/// Bumped by hand when a release changes something a running agent only
/// picks up at launch that nothing else in the stamp captures — the
/// waiter's behaviour without a change to its hook entry, say.
pub const LAUNCH_EPOCH: u32 = 1;

/// The stamp of a spawn of `definition` (the effective one) with `launch`.
pub(crate) fn stamp(
    project_root: &Path,
    definition: &str,
    launch: &LaunchConfig,
) -> Result<String> {
    let harness = launch.settings.harness;
    let flag = definition_flag(definition);
    let spec = SpawnSpec {
        definition: flag,
        append_prompt_file: None,
        prompt: None,
        resume_session: None,
        fork_session: false,
        permission_mode: launch.settings.permission_mode.as_deref(),
        model: launch.settings.model.as_deref(),
        writable_dirs: &launch.writable_dirs,
        edit_dirs: &launch.edit_dirs,
    };
    let definition_file = match flag {
        Some(def) => definition_file(project_root, def)?,
        None => None,
    };
    let inputs = json!({
        "epoch": LAUNCH_EPOCH,
        "harness": harness.to_string(),
        "definition": flag,
        "definition_file": definition_file.map(|text| crate::hash::sha256_hex(&text)),
        "prompt": crate::notice::compose_spawn_prompt_text(project_root)?,
        "permission_mode": launch.settings.permission_mode,
        "model": launch.settings.model,
        "harness_config": harness.config_section(&launch.harness_config),
        "command": harness.build_cmd(&spec, &launch.harness_config, &PreLaunch::default()),
        "loop": (!harness.hooks_reload_live()).then(|| hooks_install::loop_fingerprint(harness)),
    });
    Ok(crate::hash::sha256_hex(inputs.to_string().as_bytes()))
}

/// The first canonical definition file of `definition` that exists.
fn definition_file(project_root: &Path, definition: &str) -> Result<Option<Vec<u8>>> {
    let home = paths::home_dir().ok();
    for path in workflow::definition_paths(project_root, definition, home.as_deref()) {
        if path.is_absolute() && path.exists() {
            return Ok(Some(std::fs::read(path)?));
        }
    }
    Ok(None)
}

/// The stamp a spawn of agent `name` of `scope`, registered as `entry`,
/// would write now.
pub fn current(
    project_root: &Path,
    scope: &str,
    name: &str,
    entry: &AgentEntry,
    config: &ProjectConfig,
    global: &GlobalConfig,
) -> Result<String> {
    let definition = entry.effective_definition(name);
    let launch = resolve_launch(project_root, scope, definition, config, global)?;
    stamp(project_root, definition, &launch)
}

/// Whether agent `name` of `scope`, registered as `entry`, would launch
/// differently now than it did (module docs).
pub fn is_stale(
    project_root: &Path,
    scope: &str,
    name: &str,
    entry: &AgentEntry,
    config: &ProjectConfig,
    global: &GlobalConfig,
) -> Result<bool> {
    let Some(had) = runtime::read_launch_stamp(project_root, scope, name) else {
        return Ok(true);
    };
    Ok(current(project_root, scope, name, entry, config, global)? != had)
}
