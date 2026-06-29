//! Workflow definitions live under `<project>/.pm/workflows/<name>/`.
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
    /// Load a workflow's `config.toml`. Errors if the file is missing or malformed.
    pub fn load(project_root: &Path, name: &str) -> Result<Self> {
        let path = config_path(project_root, name);
        if !path.exists() {
            return Err(PmError::WorkflowNotFound(name.to_string()));
        }
        let content = std::fs::read_to_string(&path)?;
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
    ///   1. every member of the effective team has a definition file
    ///      resolvable from the main worktree or the global agents dir, and
    ///   2. every `brief_agents` entry is a member of the effective team.
    ///
    /// The feature worktree typically doesn't exist yet when this runs, so
    /// it isn't consulted.
    pub fn validate(&self, project_root: &Path, workflow_name: &str) -> Result<()> {
        self.validate_with_home(project_root, workflow_name, dirs::home_dir().as_deref())
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
            if !definition_exists(project_root, agent, home) {
                let (main_def, global_def) = definition_paths(project_root, agent, home);
                return Err(PmError::WorkflowAgentMissing {
                    workflow: workflow_name.to_string(),
                    agent: agent.clone(),
                    main_def,
                    global_def,
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

/// Path to a workflow's `config.toml`.
pub fn config_path(project_root: &Path, name: &str) -> PathBuf {
    paths::workflows_dir(project_root)
        .join(name)
        .join("config.toml")
}

/// Path to a workflow's `workflow.md` (the prose dumped by `pm workflow show`).
pub fn workflow_md_path(project_root: &Path, name: &str) -> PathBuf {
    paths::workflows_dir(project_root)
        .join(name)
        .join("workflow.md")
}

/// Check whether a workflow's directory exists (contains `config.toml`).
pub fn exists(project_root: &Path, name: &str) -> bool {
    config_path(project_root, name).is_file()
}

/// Outcome of [`list_installed_with_errors`]: the successfully-parsed
/// workflows plus a per-name reason for any that couldn't be parsed.
/// Callers (e.g. `pm workflow list`) typically print the successes to
/// stdout and the errors to stderr so neither hides the other.
pub struct InstalledWorkflows {
    pub workflows: Vec<(String, WorkflowDef)>,
    pub errors: Vec<(String, String)>,
}

/// List installed workflows (those with a parseable `config.toml`).
/// Returns sorted `(name, def)` pairs. Skips entries that fail to parse.
/// Use [`list_installed_with_errors`] if you also want to surface parse
/// failures to the caller.
pub fn list_installed(project_root: &Path) -> Result<Vec<(String, WorkflowDef)>> {
    Ok(list_installed_with_errors(project_root)?.workflows)
}

/// Same as [`list_installed`] but also returns parse errors so the
/// caller can warn the user about broken `config.toml` files.
pub fn list_installed_with_errors(project_root: &Path) -> Result<InstalledWorkflows> {
    let dir = paths::workflows_dir(project_root);
    if !dir.exists() {
        return Ok(InstalledWorkflows {
            workflows: Vec::new(),
            errors: Vec::new(),
        });
    }
    let mut workflows = Vec::new();
    let mut errors = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        match WorkflowDef::load(project_root, &name) {
            Ok(def) => workflows.push((name, def)),
            Err(e) => errors.push((name, e.to_string())),
        }
    }
    workflows.sort_by(|a, b| a.0.cmp(&b.0));
    errors.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(InstalledWorkflows { workflows, errors })
}

/// The two candidate locations for an agent definition file: the main
/// worktree's `.claude/agents/<name>.md`, then the global
/// `<home>/.claude/agents/<name>.md`. When `home` is `None` the global path
/// is a `~/...` placeholder used only for error messages. Shared so callers
/// resolving `--agent <def>` report the same paths the validator checks.
pub fn definition_paths(
    project_root: &Path,
    agent: &str,
    home: Option<&Path>,
) -> (PathBuf, PathBuf) {
    let filename = format!("{agent}.md");
    let main_def = paths::main_worktree(project_root)
        .join(".claude/agents")
        .join(&filename);
    let global_def = home
        .map(|h| h.join(".claude/agents").join(&filename))
        .unwrap_or_else(|| PathBuf::from("~/.claude/agents/<name>.md"));
    (main_def, global_def)
}

/// True iff an agent definition file is resolvable from the main worktree
/// or the supplied home directory. The feature worktree is intentionally
/// not consulted — at `feat new` time it doesn't exist yet.
pub fn definition_exists(project_root: &Path, agent: &str, home: Option<&Path>) -> bool {
    let (main_def, global_def) = definition_paths(project_root, agent, home);
    main_def.exists() || (home.is_some() && global_def.exists())
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
        let main_agents = paths::main_worktree(dir.path()).join(".claude/agents");
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
        // on a user's real `~/.claude/agents/frontend-impl.md`.
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
        let main_agents = paths::main_worktree(dir.path()).join(".claude/agents");
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
        let main_agents = paths::main_worktree(dir.path()).join(".claude/agents");
        std::fs::create_dir_all(&main_agents).unwrap();
        std::fs::write(main_agents.join("implementer.md"), "stub").unwrap();

        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        def.validate_with_home(dir.path(), "demo", Some(dir.path()))
            .unwrap();
    }

    #[test]
    fn list_installed_returns_sorted() {
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "beta",
            r#"description = "b"
"#,
        );
        write_workflow(
            dir.path(),
            "alpha",
            r#"description = "a"
"#,
        );
        let list = list_installed(dir.path()).unwrap();
        let names: Vec<_> = list.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn list_installed_empty_when_no_workflows_dir() {
        let dir = tempdir().unwrap();
        let list = list_installed(dir.path()).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn list_installed_skips_unparseable_workflows() {
        let dir = tempdir().unwrap();
        let workflows_dir = paths::workflows_dir(dir.path());
        std::fs::create_dir_all(workflows_dir.join("bad")).unwrap();
        std::fs::write(workflows_dir.join("bad").join("config.toml"), "not toml{{{").unwrap();
        write_workflow(
            dir.path(),
            "good",
            r#"description = "ok"
"#,
        );
        let list = list_installed(dir.path()).unwrap();
        let names: Vec<_> = list.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["good"]);
    }
}
