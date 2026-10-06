//! What config a spawn launches with: the agent's settings, harness config
//! and the directories it may write outside its worktree.

use std::path::Path;

use crate::error::Result;
use crate::harness::Harness;
use crate::state::paths;
use crate::state::project::{
    AgentSettings, AgentsConfig, GlobalConfig, HarnessConfig, ProjectConfig,
    resolve_agent_settings, resolve_harness_config,
};
use crate::state::workflow;

/// The agent definition a spawn keys on — for the harness's definition flag
/// and for the per-agent settings lookup alike. The explicit override wins;
/// otherwise the display name doubles as the definition (back-compat).
pub(super) fn effective_definition<'a>(
    agent_definition: Option<&'a str>,
    agent_name: Option<&'a str>,
) -> Option<&'a str> {
    agent_definition.or(agent_name)
}

/// The settings a spawn actually launches with: config resolved for this
/// agent's definition. A `None` definition (a plain, unregistered session)
/// takes no config at all.
fn spawn_settings(
    definition: Option<&str>,
    project: &AgentsConfig,
    global: &AgentsConfig,
) -> Result<AgentSettings> {
    Ok(definition
        .map(|def| resolve_agent_settings(project, global, def))
        .transpose()?
        .unwrap_or_default())
}

/// The harness config selects for `definition`, checked before a respawn
/// or fork so a stored session id from a different harness isn't resumed.
pub(crate) fn configured_harness(
    definition: &str,
    project: &AgentsConfig,
    global: &AgentsConfig,
) -> Result<Harness> {
    Ok(resolve_agent_settings(project, global, definition)?.harness)
}

/// Directories outside the worktree an agent must be able to write, for a
/// harness that sandboxes: pm's state, the shared `.git` every worktree
/// writes through, the pm config dir, plus any `[harness.codex]
/// writable_roots` (relative ones resolve against the project root).
pub(super) fn writable_dirs(
    project_root: &Path,
    config: &HarnessConfig,
) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![
        paths::pm_dir(project_root),
        paths::main_worktree(project_root).join(".git"),
    ];
    dirs.extend(paths::global_config_dir().ok());
    for root in config.codex.writable_roots.as_deref().unwrap_or(&[]) {
        let path = Path::new(root);
        dirs.push(if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        });
    }
    dirs
}

/// The definition that reaches the harness: the effective definition, except
/// the reserved vanilla name (any alias), which launches a definition-less
/// session even if a matching definition file happens to exist.
pub(crate) fn definition_flag(effective_definition: Option<&str>) -> Option<&str> {
    effective_definition.filter(|d| !workflow::is_vanilla(d))
}

/// What config has a spawn of `definition` (the effective one) in `scope`
/// launch with: the one resolution a spawn and its
/// [`launch_stamp`](crate::commands::launch_stamp) share.
pub(crate) struct LaunchConfig {
    /// Re-resolved from config on every spawn, never stored on the registry
    /// entry, so restart, fork and heal pick up config edits.
    pub settings: AgentSettings,
    pub harness_config: HarnessConfig,
    pub writable_dirs: Vec<std::path::PathBuf>,
    pub edit_dirs: Vec<std::path::PathBuf>,
}

pub(crate) fn resolve_launch(
    project_root: &Path,
    scope: &str,
    definition: Option<&str>,
    config: &ProjectConfig,
    global: &GlobalConfig,
) -> Result<LaunchConfig> {
    let settings = spawn_settings(definition, &config.agents, &global.agents)?;
    let harness_config = resolve_harness_config(&config.harness, &global.harness);
    let writable_dirs = writable_dirs(project_root, &harness_config);
    let edit_dirs = if scope == "main" {
        Vec::new()
    } else {
        vec![paths::summaries_dir(project_root)]
    };
    Ok(LaunchConfig {
        settings,
        harness_config,
        writable_dirs,
        edit_dirs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{self, Harness, SpawnSpec};
    use crate::state::project::{AgentSettings, AgentsConfig, HarnessConfig};
    use std::path::PathBuf;

    #[test]
    fn vanilla_agent_gets_no_definition_flag() {
        // The reserved name is filtered out of the definition flag; any
        // other definition passes through.
        let alias = "plain";
        assert_eq!(definition_flag(Some(alias)), None, "{alias}");
        let cmd = Harness::ClaudeCode.build_cmd(
            &SpawnSpec {
                definition: definition_flag(Some(alias)),
                ..Default::default()
            },
            &HarnessConfig::default(),
            &harness::PreLaunch::default(),
        );
        assert!(
            !cmd.contains("--agent"),
            "vanilla spawn must not pass --agent, got: {cmd}"
        );
        assert_eq!(definition_flag(Some("reviewer")), Some("reviewer"));
    }

    #[test]
    fn writable_dirs_are_pm_state_shared_git_config_dir_and_configured_roots() {
        let root = Path::new("/proj");
        let config = HarnessConfig {
            codex: crate::state::project::CodexConfig {
                writable_roots: Some(vec!["/abs/cache".into(), "main/target".into()]),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            writable_dirs(root, &config),
            vec![
                PathBuf::from("/proj/.pm"),
                PathBuf::from("/proj/main/.git"),
                paths::global_config_dir().unwrap(),
                PathBuf::from("/abs/cache"),
                PathBuf::from("/proj/main/target"),
            ]
        );
    }

    #[test]
    fn spawn_settings_keyed_by_definition_not_display_name() {
        // A named agent (display `backend-dev`, definition `implementer`)
        // takes the definition's settings, not the display name's.
        let project = AgentsConfig {
            models: [
                ("implementer".to_string(), "opus".to_string()),
                ("backend-dev".to_string(), "haiku".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let named = effective_definition(Some("implementer"), Some("backend-dev"));
        let settings = spawn_settings(named, &project, &AgentsConfig::default()).unwrap();
        assert_eq!(settings.model.as_deref(), Some("opus"));

        // With no override the display name doubles as the definition.
        let plain = effective_definition(None, Some("backend-dev"));
        let settings = spawn_settings(plain, &project, &AgentsConfig::default()).unwrap();
        assert_eq!(settings.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn spawn_settings_without_definition_takes_no_config() {
        // A plain claude session has no definition to key on.
        let project = AgentsConfig {
            permissions: [("claude".to_string(), "plan".to_string())]
                .into_iter()
                .collect(),
            models: [("claude".to_string(), "opus".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let settings = spawn_settings(None, &project, &AgentsConfig::default()).unwrap();
        assert_eq!(settings, AgentSettings::default());
    }
}
