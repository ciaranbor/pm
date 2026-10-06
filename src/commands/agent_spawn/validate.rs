//! The pre-spawn check that an agent definition resolves.

use std::path::Path;

use crate::commands::bundled_disable::Disabled;
use crate::error::{PmError, Result};
use crate::state::paths;
use crate::state::workflow;

/// Pre-spawn check that the agent definition resolves to a real file, so a
/// typo'd or nonexistent `--agent <def>` fails loudly instead of printing
/// "Spawned …" over a tmux window whose harness errors out immediately — the
/// spawn is fire-and-forget, so that failure is invisible.
///
/// The `_with_home` split exists so resolution can be unit-tested against an
/// explicit home rather than the process's `$HOME`.
pub(crate) fn validate_definition_resolves(project_root: &Path, definition: &str) -> Result<()> {
    validate_definition_resolves_with_home(
        project_root,
        definition,
        paths::home_dir().ok().as_deref(),
        &Disabled::load(),
    )
}

fn validate_definition_resolves_with_home(
    project_root: &Path,
    definition: &str,
    home: Option<&Path>,
    disabled: &Disabled,
) -> Result<()> {
    // The reserved vanilla name spawns with no definition — no file to check.
    if workflow::is_vanilla(definition) {
        return Ok(());
    }
    if workflow::definition_exists(project_root, definition, home) {
        return Ok(());
    }
    if definition == workflow::LEGACY_VANILLA_AGENT
        && !crate::commands::vanilla_rename::is_migrated(project_root)
    {
        return Err(PmError::UnmigratedVanillaAgent);
    }
    Err(disabled.explain(
        project_root,
        PmError::AgentDefinitionMissing {
            agent: definition.to_string(),
            searched: workflow::definition_paths(project_root, definition, home),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    #[test]
    fn validate_definition_resolves_with_home_pass_and_fail() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let home = tempdir().unwrap();

        // Missing everywhere; present only in the global agents dir; present
        // only in the main worktree (home = None) — each must resolve correctly.
        assert!(matches!(
            validate_definition_resolves_with_home(
                project_root,
                "impl",
                Some(home.path()),
                &Disabled::default()
            )
            .unwrap_err(),
            PmError::AgentDefinitionMissing { .. }
        ));

        let global = home.path().join(".agents/agents");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(global.join("impl.md"), "# stub").unwrap();
        validate_definition_resolves_with_home(
            project_root,
            "impl",
            Some(home.path()),
            &Disabled::default(),
        )
        .unwrap();

        let main = paths::main_worktree(project_root).join(".agents/agents");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("other.md"), "# stub").unwrap();
        validate_definition_resolves_with_home(project_root, "other", None, &Disabled::default())
            .unwrap();
    }

    #[test]
    fn a_disabled_bundled_definition_is_refused_unless_a_project_custom_resolves() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let home = tempdir().unwrap();
        let disabled = Disabled::from_config(toml::from_str("agents = [\"qa\"]").unwrap());

        let err = validate_definition_resolves_with_home(
            project_root,
            "qa",
            Some(home.path()),
            &disabled,
        )
        .unwrap_err();
        assert!(matches!(err, PmError::BundledDisabled { .. }), "{err}");

        let main = paths::main_worktree(project_root).join(".agents/agents");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("qa.md"), "# my qa").unwrap();
        validate_definition_resolves_with_home(project_root, "qa", Some(home.path()), &disabled)
            .unwrap();
    }

    #[test]
    fn vanilla_agent_skips_definition_validation() {
        // `pm agent spawn plain` must work with no def anywhere.
        let tmp = tempfile::tempdir().unwrap();
        validate_definition_resolves_with_home(tmp.path(), "plain", None, &Disabled::default())
            .unwrap();
    }

    #[test]
    fn legacy_vanilla_name_needs_a_definition_and_hints_upgrade_until_migrated() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            validate_definition_resolves_with_home(
                tmp.path(),
                "default",
                None,
                &Disabled::default()
            ),
            Err(PmError::UnmigratedVanillaAgent)
        ));
        crate::commands::vanilla_rename::write_marker(tmp.path()).unwrap();
        assert!(matches!(
            validate_definition_resolves_with_home(
                tmp.path(),
                "default",
                None,
                &Disabled::default()
            ),
            Err(PmError::AgentDefinitionMissing { .. })
        ));
    }
}
