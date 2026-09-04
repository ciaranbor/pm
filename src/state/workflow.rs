//! Workflow definitions live in two tiers, resolved project-first by name:
//! `<project>/.pm/workflows/<name>/` (the project's customs) and the global
//! `<pm config dir>/workflows/<name>/` (where the bundled ones install, plus
//! any global customs under non-bundled names).
//!
//! Each workflow directory contains:
//! - `config.toml` — machine-readable: description, agents, brief_agents list
//! - `workflow.md` — human-readable routing prose, surfaced by
//!   `pm workflow show`
//!
//! The TOML schema is intentionally minimal — v1 only uses `description`,
//! `agents`, and `brief_agents`. New fields can be added later without
//! breaking on-disk files.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{PmError, Result};
use crate::state::paths;

/// Reserved agent name meaning a definition-less vanilla harness session.
/// Unconditional: even if a `default.md` definition file exists, this name
/// spawns with no definition flag. Validation skips the definition-file
/// check for it.
pub const VANILLA_AGENT: &str = "default";

/// Every spelling of the vanilla name. `claude` was the original and stays
/// an alias for good: installed (Preserve-policy) `solo` workflows name it
/// in their `config.toml` and are never rewritten.
pub const VANILLA_AGENT_ALIASES: &[&str] = &[VANILLA_AGENT, "claude"];

/// Whether `name` is the reserved vanilla agent under any of its spellings.
pub fn is_vanilla(name: &str) -> bool {
    VANILLA_AGENT_ALIASES.contains(&name)
}

/// Parsed `<workflow>/config.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDef {
    /// One-line description, shown by `pm workflow list`.
    pub description: String,
    /// Optional hint, aimed at the `main` orchestrator, describing the
    /// situation this workflow fits. Surfaced by `pm workflow list`.
    /// Advisory metadata — a custom workflow needn't provide one.
    #[serde(default)]
    pub when_to_use: Option<String>,
    /// The full agent team for the workflow. **All** of these are spawned
    /// at `pm feat new`/`feat adopt --workflow <name>` time (with or
    /// without `--context`). Empty falls back to `brief_agents` for
    /// back-compat with custom workflows that only set the old field.
    #[serde(default)]
    pub agents: Vec<String>,
    /// Subset of the team that receives a copy of the `--context` brief.
    /// Spawning is *not* its job — that's `agents`. Accepts the legacy
    /// `auto_spawn` key so already-installed configs keep parsing.
    #[serde(default, alias = "auto_spawn")]
    pub brief_agents: Vec<String>,
}

impl WorkflowDef {
    /// Load a workflow's `config.toml` from whichever tier resolves it.
    /// Errors if neither has it or the file is malformed.
    pub fn load(project_root: &Path, name: &str) -> Result<Self> {
        Self::load_with_global(Some(project_root), name, &global_dir()?)
    }

    /// [`load`](Self::load) against an explicit global tier; `project_root`
    /// `None` consults the global tier only.
    pub fn load_with_global(
        project_root: Option<&Path>,
        name: &str,
        global_dir: &Path,
    ) -> Result<Self> {
        let (dir, _) = resolve_dir(project_root, name, global_dir)
            .ok_or_else(|| PmError::WorkflowNotFound(name.to_string()))?;
        Self::load_from_dir(&dir)
    }

    fn load_from_dir(dir: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(dir.join("config.toml"))?;
        let def: Self = toml::from_str(&content)?;
        Ok(def)
    }

    /// The effective spawn set: the full `agents` team, or — when that's
    /// empty — `brief_agents` (back-compat for custom workflows that only
    /// set the old field). This is the set pm spawns at feature creation.
    pub fn effective_team(&self) -> &[String] {
        if self.agents.is_empty() {
            &self.brief_agents
        } else {
            &self.agents
        }
    }

    /// Validate the workflow's spawn set:
    ///   1. every member of the effective team has a definition file in a
    ///      canonical store (see [`definition_paths`]), and
    ///   2. every `brief_agents` entry is a member of the effective team.
    ///
    /// The feature worktree typically doesn't exist yet when this runs, so
    /// it isn't consulted.
    pub fn validate(&self, project_root: &Path, workflow_name: &str) -> Result<()> {
        self.validate_with_home(
            project_root,
            workflow_name,
            paths::home_dir().ok().as_deref(),
        )
    }

    /// Test-friendly variant of [`validate`] that takes an explicit home
    /// directory instead of reading `$HOME` from the process environment.
    /// Production callers should use [`validate`]; tests use this to avoid
    /// races on process-global `$HOME`.
    pub fn validate_with_home(
        &self,
        project_root: &Path,
        workflow_name: &str,
        home: Option<&Path>,
    ) -> Result<()> {
        let team = self.effective_team();
        for agent in team {
            // The reserved vanilla name needs no definition file.
            if is_vanilla(agent) {
                continue;
            }
            if !definition_exists(project_root, agent, home) {
                return Err(PmError::WorkflowAgentMissing {
                    workflow: workflow_name.to_string(),
                    agent: agent.clone(),
                    searched: definition_paths(project_root, agent, home),
                });
            }
        }
        // Every brief recipient must be part of the spawned team — a brief
        // sent to an agent that never spawns would be silently swallowed.
        for agent in &self.brief_agents {
            if !team.contains(agent) {
                return Err(PmError::SafetyCheck(format!(
                    "workflow '{workflow_name}' lists '{agent}' in `brief_agents` but not in \
                     `agents`. Every brief recipient must be a member of the spawned team."
                )));
            }
        }
        Ok(())
    }
}

/// Which tier a workflow resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Project,
    Global,
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Tier::Project => "project",
            Tier::Global => "global",
        })
    }
}

/// The global workflow tier for this process.
pub fn global_dir() -> Result<PathBuf> {
    paths::global_workflows_dir()
}

/// The directory holding workflow `name` and the tier it came from: the
/// project's `.pm/workflows/<name>/` when that has a `config.toml`, else
/// `<global_dir>/<name>/`. `None` when neither does.
pub fn resolve_dir(
    project_root: Option<&Path>,
    name: &str,
    global_dir: &Path,
) -> Option<(PathBuf, Tier)> {
    if let Some(root) = project_root {
        let dir = paths::workflows_dir(root).join(name);
        if dir.join("config.toml").is_file() {
            return Some((dir, Tier::Project));
        }
    }
    let dir = global_dir.join(name);
    dir.join("config.toml")
        .is_file()
        .then_some((dir, Tier::Global))
}

/// Path to a workflow's `workflow.md` (the prose dumped by `pm workflow
/// show`) in whichever tier resolves it; `None` if the workflow isn't
/// installed. The file itself may still be missing.
pub fn workflow_md_path(project_root: &Path, name: &str) -> Option<PathBuf> {
    let global = global_dir().ok()?;
    resolve_dir(Some(project_root), name, &global).map(|(dir, _)| dir.join("workflow.md"))
}

/// Whether a workflow is installed in either tier.
pub fn exists(project_root: &Path, name: &str) -> bool {
    global_dir()
        .ok()
        .is_some_and(|g| resolve_dir(Some(project_root), name, &g).is_some())
}

/// A parsed workflow and where it resolved from.
pub struct InstalledWorkflow {
    pub name: String,
    pub def: WorkflowDef,
    pub tier: Tier,
}

/// Outcome of [`list_installed_with_errors`]: the successfully-parsed
/// workflows plus a per-name reason for any that couldn't be parsed.
/// Callers (e.g. `pm workflow list`) typically print the successes to
/// stdout and the errors to stderr so neither hides the other.
pub struct InstalledWorkflows {
    pub workflows: Vec<InstalledWorkflow>,
    pub errors: Vec<(String, String)>,
}

/// Every installed workflow across both tiers, sorted by name; a project
/// entry shadows a same-named global one. `project_root` `None` lists the
/// global tier only. Parse failures are returned alongside so the caller
/// can warn about broken `config.toml` files.
pub fn list_installed_with_errors(project_root: Option<&Path>) -> Result<InstalledWorkflows> {
    list_installed_in(project_root, &global_dir()?)
}

pub fn list_installed_in(
    project_root: Option<&Path>,
    global_dir: &Path,
) -> Result<InstalledWorkflows> {
    let mut names: Vec<String> = Vec::new();
    let mut dirs = vec![global_dir.to_path_buf()];
    if let Some(root) = project_root {
        dirs.push(paths::workflows_dir(root));
    }
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str()
                && !names.iter().any(|n| n == name)
            {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    let mut workflows = Vec::new();
    let mut errors = Vec::new();
    for name in names {
        let Some((dir, tier)) = resolve_dir(project_root, &name, global_dir) else {
            // A directory with no `config.toml` in the tier that would win.
            errors.push((name, "config.toml missing".to_string()));
            continue;
        };
        match WorkflowDef::load_from_dir(&dir) {
            Ok(def) => workflows.push(InstalledWorkflow { name, def, tier }),
            Err(e) => errors.push((name, e.to_string())),
        }
    }
    Ok(InstalledWorkflows { workflows, errors })
}

/// Where an agent definition file may live, in lookup order: the main
/// worktree's canonical `.agents/agents/`, then the global `~/.agents/agents/`.
/// Only the canonical stores count — a harness's own dir (`.claude/agents/`)
/// is a projection of them, never a source. When `home` is `None` the global
/// entry is a `~/…` placeholder used only for error messages. Shared so
/// callers resolving a definition report the same paths the validator checks.
pub fn definition_paths(project_root: &Path, agent: &str, home: Option<&Path>) -> Vec<PathBuf> {
    let filename = format!("{agent}.md");
    let main = paths::main_worktree(project_root);
    vec![
        main.join(".agents/agents").join(&filename),
        match home {
            Some(h) => h.join(".agents/agents").join(&filename),
            None => PathBuf::from("~/.agents/agents").join(&filename),
        },
    ]
}

/// True iff an agent definition file exists at any of [`definition_paths`].
/// The feature worktree is intentionally not consulted — at `feat new` time
/// it doesn't exist yet. `~/…` placeholders (no home) are never checked.
pub fn definition_exists(project_root: &Path, agent: &str, home: Option<&Path>) -> bool {
    definition_paths(project_root, agent, home)
        .iter()
        .any(|p| p.is_absolute() && p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_workflow(project_root: &Path, name: &str, body: &str) {
        let dir = paths::workflows_dir(project_root).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
        std::fs::write(dir.join("workflow.md"), "# md").unwrap();
    }

    #[test]
    fn parses_minimal_config_toml() {
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
agents = ["a", "b"]
brief_agents = ["a"]
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        assert_eq!(def.description, "x");
        assert_eq!(def.agents, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(def.brief_agents, vec!["a".to_string()]);
        // `when_to_use` is optional advisory metadata; absent parses as None.
        assert_eq!(def.when_to_use, None);
    }

    #[test]
    fn parses_when_to_use_when_present() {
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
when_to_use = "use it here"
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        assert_eq!(def.when_to_use.as_deref(), Some("use it here"));
    }

    #[test]
    fn parses_legacy_auto_spawn_via_serde_alias() {
        // Already-installed (Preserve-policy) configs still say `auto_spawn`.
        // The serde alias keeps them parsing into `brief_agents`.
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
agents = ["a", "b"]
auto_spawn = ["a"]
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        assert_eq!(def.brief_agents, vec!["a".to_string()]);
    }

    #[test]
    fn effective_team_falls_back_to_brief_agents_when_agents_empty() {
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
auto_spawn = ["a", "b"]
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        assert!(def.agents.is_empty());
        assert_eq!(def.effective_team(), &["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn effective_team_is_agents_when_present() {
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
agents = ["a", "b", "c"]
brief_agents = ["a"]
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        assert_eq!(
            def.effective_team(),
            &["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn errors_on_missing_description() {
        let dir = tempdir().unwrap();
        write_workflow(dir.path(), "demo", "agents = [\"a\"]\n");
        let result = WorkflowDef::load(dir.path(), "demo");
        assert!(result.is_err());
    }

    #[test]
    fn empty_optional_lists_default_to_empty() {
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        assert!(def.agents.is_empty());
        assert!(def.brief_agents.is_empty());
    }

    #[test]
    fn load_missing_workflow_returns_workflow_not_found() {
        let dir = tempdir().unwrap();
        let err = WorkflowDef::load(dir.path(), "missing").unwrap_err();
        assert!(matches!(err, PmError::WorkflowNotFound(_)));
    }

    #[test]
    fn validate_ok_when_definition_in_main() {
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
agents = ["implementer"]
brief_agents = ["implementer"]
"#,
        );
        let main_agents = paths::main_worktree(dir.path()).join(".agents/agents");
        std::fs::create_dir_all(&main_agents).unwrap();
        std::fs::write(main_agents.join("implementer.md"), "stub").unwrap();

        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        def.validate(dir.path(), "demo").unwrap();
    }

    #[test]
    fn validate_errors_when_team_member_definition_missing() {
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
agents = ["frontend-impl"]
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        // Use the explicit-home variant so the test never mutates process
        // env. Pointing the home at our tempdir guarantees no spurious hit
        // on a user's real `~/.agents/agents/frontend-impl.md`.
        let result = def.validate_with_home(dir.path(), "demo", Some(dir.path()));
        assert!(matches!(
            result.unwrap_err(),
            PmError::WorkflowAgentMissing { .. }
        ));
    }

    #[test]
    fn validate_errors_when_home_is_none() {
        // When `home` is None, `validate` should also fail cleanly if main
        // has no matching definition.
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
agents = ["frontend-impl"]
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        let result = def.validate_with_home(dir.path(), "demo", None);
        assert!(matches!(
            result.unwrap_err(),
            PmError::WorkflowAgentMissing { .. }
        ));
    }

    #[test]
    fn validate_skips_definition_check_for_vanilla_agent_aliases() {
        // Both spellings of the reserved vanilla name mean a definition-less
        // session — validation must pass with no def file anywhere.
        for name in ["default", "claude"] {
            let dir = tempdir().unwrap();
            write_workflow(
                dir.path(),
                "demo",
                &format!(
                    r#"description = "x"
agents = ["{name}"]
brief_agents = ["{name}"]
"#
                ),
            );
            let def = WorkflowDef::load(dir.path(), "demo").unwrap();
            // Home pointed at the empty tempdir: no definition can resolve.
            def.validate_with_home(dir.path(), "demo", Some(dir.path()))
                .unwrap();
        }
    }

    #[test]
    fn validate_resolves_definition_from_either_canonical_store() {
        // The main worktree's `.agents/agents/` and the home one are each
        // sufficient; a harness's own dir is not a source.
        for (in_home, rel, ok) in [
            (false, ".agents/agents", true),
            (true, ".agents/agents", true),
            (false, ".claude/agents", false),
            (true, ".claude/agents", false),
        ] {
            let dir = tempdir().unwrap();
            let home = tempdir().unwrap();
            write_workflow(
                dir.path(),
                "demo",
                r#"description = "x"
agents = ["impl"]
"#,
            );
            let base = if in_home {
                home.path().to_path_buf()
            } else {
                paths::main_worktree(dir.path())
            };
            std::fs::create_dir_all(base.join(rel)).unwrap();
            std::fs::write(base.join(rel).join("impl.md"), "stub").unwrap();
            let def = WorkflowDef::load(dir.path(), "demo").unwrap();
            let result = def.validate_with_home(dir.path(), "demo", Some(home.path()));
            assert_eq!(result.is_ok(), ok, "{rel} (home={in_home}): {result:?}");
        }
    }

    #[test]
    fn missing_definition_error_lists_every_searched_path() {
        let dir = tempdir().unwrap();
        let home = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
agents = ["ghost"]
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        let err = def
            .validate_with_home(dir.path(), "demo", Some(home.path()))
            .unwrap_err()
            .to_string();
        for p in definition_paths(dir.path(), "ghost", Some(home.path())) {
            assert!(err.contains(&p.display().to_string()), "{err}");
        }
        assert!(err.contains(".agents/agents/ghost.md"));
        assert!(!err.contains(".claude"));
    }

    #[test]
    fn validate_errors_when_brief_agent_not_in_team() {
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
agents = ["implementer"]
brief_agents = ["reviewer"]
"#,
        );
        let main_agents = paths::main_worktree(dir.path()).join(".agents/agents");
        std::fs::create_dir_all(&main_agents).unwrap();
        std::fs::write(main_agents.join("implementer.md"), "stub").unwrap();

        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        let result = def.validate_with_home(dir.path(), "demo", Some(dir.path()));
        assert!(matches!(result.unwrap_err(), PmError::SafetyCheck(_)));
    }

    #[test]
    fn validate_uses_brief_agents_fallback_when_agents_empty() {
        // Custom workflow with only the legacy field: the effective team is
        // `brief_agents`, so its members must have definitions.
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
auto_spawn = ["implementer"]
"#,
        );
        let main_agents = paths::main_worktree(dir.path()).join(".agents/agents");
        std::fs::create_dir_all(&main_agents).unwrap();
        std::fs::write(main_agents.join("implementer.md"), "stub").unwrap();

        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        def.validate_with_home(dir.path(), "demo", Some(dir.path()))
            .unwrap();
    }

    fn write_global_workflow(global_dir: &Path, name: &str, body: &str) {
        let dir = global_dir.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
        std::fs::write(dir.join("workflow.md"), "# global md").unwrap();
    }

    #[test]
    fn resolve_prefers_project_tier_and_falls_back_to_global() {
        let project = tempdir().unwrap();
        let global = tempdir().unwrap();
        write_global_workflow(global.path(), "solo", "description = \"bundled\"\n");
        write_global_workflow(global.path(), "shared", "description = \"global custom\"\n");
        write_workflow(
            project.path(),
            "solo",
            "description = \"project override\"\n",
        );

        let (dir, tier) = resolve_dir(Some(project.path()), "solo", global.path()).unwrap();
        assert_eq!(tier, Tier::Project);
        assert_eq!(dir, paths::workflows_dir(project.path()).join("solo"));
        let (dir, tier) = resolve_dir(Some(project.path()), "shared", global.path()).unwrap();
        assert_eq!(tier, Tier::Global);
        assert_eq!(dir, global.path().join("shared"));
        assert!(resolve_dir(Some(project.path()), "ghost", global.path()).is_none());
        // No project: the global tier alone.
        assert_eq!(
            resolve_dir(None, "solo", global.path()).unwrap().1,
            Tier::Global
        );

        let def =
            WorkflowDef::load_with_global(Some(project.path()), "solo", global.path()).unwrap();
        assert_eq!(def.description, "project override");
        let def = WorkflowDef::load_with_global(None, "solo", global.path()).unwrap();
        assert_eq!(def.description, "bundled");
        let err = WorkflowDef::load_with_global(Some(project.path()), "ghost", global.path())
            .unwrap_err();
        assert!(matches!(err, PmError::WorkflowNotFound(_)));

        let list = list_installed_in(Some(project.path()), global.path()).unwrap();
        let rows: Vec<(&str, Tier)> = list
            .workflows
            .iter()
            .map(|w| (w.name.as_str(), w.tier))
            .collect();
        assert_eq!(
            rows,
            vec![("shared", Tier::Global), ("solo", Tier::Project)]
        );
        assert!(list.errors.is_empty());
        let global_only = list_installed_in(None, global.path()).unwrap();
        assert!(global_only.workflows.iter().all(|w| w.tier == Tier::Global));
    }

    #[test]
    fn list_installed_empty_when_neither_tier_exists() {
        let dir = tempdir().unwrap();
        let global = tempdir().unwrap();
        let list = list_installed_in(Some(dir.path()), &global.path().join("none")).unwrap();
        assert!(list.workflows.is_empty() && list.errors.is_empty());
    }

    #[test]
    fn list_installed_reports_unparseable_workflows_as_errors() {
        let dir = tempdir().unwrap();
        let global = tempdir().unwrap();
        let workflows_dir = paths::workflows_dir(dir.path());
        std::fs::create_dir_all(workflows_dir.join("bad")).unwrap();
        std::fs::write(workflows_dir.join("bad").join("config.toml"), "not toml{{{").unwrap();
        write_workflow(dir.path(), "good", "description = \"ok\"\n");
        let list = list_installed_in(Some(dir.path()), global.path()).unwrap();
        let names: Vec<_> = list.workflows.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["good"]);
        assert_eq!(list.errors.len(), 1);
        assert_eq!(list.errors[0].0, "bad");
    }
}
