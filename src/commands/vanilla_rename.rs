//! The vanilla agent's rename from `default` to `plain`.
//!
//! An agent registered as `default` was launched vanilla; once `default` is
//! an ordinary name, restarting or healing it would look for a `default`
//! definition. `pm upgrade` migrates each project once: every such entry
//! keeps its name (window, `PM_AGENT_NAME`, inbox) and records `plain` as its
//! definition, so a running agent is untouched and its respawns stay
//! vanilla. The migration is one-shot (a marker under `.pm/migrations/`)
//! because afterwards `default` may be given a real definition.

use std::path::Path;

use crate::error::Result;
use crate::fs_utils::write_atomic;
use crate::state::agent::{AgentRegistry, AgentType};
use crate::state::paths;
use crate::state::workflow::{LEGACY_VANILLA_AGENT, VANILLA_AGENT};

const MARKER: &str = "plain-agent";

/// Whether this project's registry entries have been migrated; until then a
/// `default` entry is a vanilla agent spawned under the old name.
pub fn is_migrated(project_root: &Path) -> bool {
    paths::migration_marker(project_root, MARKER).exists()
}

pub fn write_marker(project_root: &Path) -> Result<()> {
    write_atomic(&paths::migration_marker(project_root, MARKER), b"")
}

/// `(scope, agent)` for every registered agent whose effective definition is
/// the old vanilla name, in scope order, skipping unreadable registries.
/// Meaningful only before migration.
pub fn legacy_agents(project_root: &Path) -> Result<Vec<(String, String)>> {
    let agents_dir = paths::agents_dir(project_root);
    let mut out = Vec::new();
    for scope in registry_scopes(&agents_dir)? {
        // A registry that doesn't parse can't spawn anything to migrate, and
        // must not fail the rest of `pm upgrade`.
        let Ok(registry) = AgentRegistry::load(&agents_dir, &scope) else {
            continue;
        };
        for (name, entry) in &registry.agents {
            if entry.agent_type == AgentType::Agent
                && entry.effective_definition(name) == LEGACY_VANILLA_AGENT
            {
                out.push((scope.clone(), name.clone()));
            }
        }
    }
    Ok(out)
}

/// Point every legacy vanilla entry at `plain` and write the marker, unless
/// already migrated. Returns the `(scope, agent)` pairs changed (or, with
/// `dry_run`, that would be), writing nothing on a dry run.
pub fn migrate(project_root: &Path, dry_run: bool) -> Result<Vec<(String, String)>> {
    if is_migrated(project_root) {
        return Ok(Vec::new());
    }
    let legacy = legacy_agents(project_root)?;
    if dry_run {
        return Ok(legacy);
    }
    let agents_dir = paths::agents_dir(project_root);
    let mut scopes: Vec<&str> = legacy.iter().map(|(s, _)| s.as_str()).collect();
    scopes.dedup();
    for scope in scopes {
        let mut registry = AgentRegistry::load(&agents_dir, scope)?;
        for (s, name) in &legacy {
            if s == scope
                && let Some(entry) = registry.get_mut(name)
            {
                entry.agent_definition = Some(VANILLA_AGENT.to_string());
            }
        }
        registry.save(&agents_dir, scope)?;
    }
    write_marker(project_root)?;
    Ok(legacy)
}

/// The scopes with a registry file, sorted.
fn registry_scopes(agents_dir: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(agents_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut scopes = Vec::new();
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(scope) = name.strip_suffix(".toml")
            && !scope.starts_with('.')
        {
            scopes.push(scope.to_string());
        }
    }
    scopes.sort();
    Ok(scopes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Harness;
    use crate::state::agent::AgentEntry;
    use tempfile::tempdir;

    fn entry(definition: Option<&str>) -> AgentEntry {
        AgentEntry {
            agent_type: AgentType::Agent,
            session_id: "sid".to_string(),
            window_name: String::new(),
            active: true,
            agent_definition: definition.map(str::to_string),
            harness: Harness::ClaudeCode,
            spawned_at: None,
        }
    }

    #[test]
    fn migrate_keeps_the_name_and_points_legacy_entries_at_plain_once() {
        let dir = tempdir().unwrap();
        let agents_dir = paths::agents_dir(dir.path());
        let mut login = AgentRegistry::default();
        login.register("default", entry(None));
        login.register("solo-dev", entry(Some("default")));
        login.register("implementer", entry(None));
        login.save(&agents_dir, "login").unwrap();
        let mut main = AgentRegistry::default();
        main.register("main", entry(None));
        main.save(&agents_dir, "main").unwrap();

        let planned = migrate(dir.path(), true).unwrap();
        assert!(!is_migrated(dir.path()));
        assert_eq!(
            AgentRegistry::load(&agents_dir, "login").unwrap(),
            login,
            "a dry run writes nothing"
        );

        let changed = migrate(dir.path(), false).unwrap();
        assert_eq!(changed, planned);
        assert_eq!(
            changed,
            vec![
                ("login".to_string(), "default".to_string()),
                ("login".to_string(), "solo-dev".to_string()),
            ]
        );
        let after = AgentRegistry::load(&agents_dir, "login").unwrap();
        for name in ["default", "solo-dev"] {
            let e = after.get(name).unwrap();
            assert_eq!(e.effective_definition(name), VANILLA_AGENT);
            assert_eq!(e.session_id, "sid");
        }
        assert_eq!(
            after.get("implementer").unwrap().agent_definition,
            None,
            "other entries are untouched"
        );

        // Once migrated, `default` is an ordinary name a user may define.
        let mut later = after.clone();
        later.register("default", entry(None));
        later.save(&agents_dir, "login").unwrap();
        assert!(migrate(dir.path(), false).unwrap().is_empty());
        assert_eq!(
            AgentRegistry::load(&agents_dir, "login")
                .unwrap()
                .get("default")
                .unwrap()
                .agent_definition,
            None
        );
    }
}
