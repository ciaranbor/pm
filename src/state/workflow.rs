//! Workflow definitions live under `<project>/.pm/workflows/<name>/`.
//!
//! Each workflow directory contains:
//! - `config.toml` — machine-readable: description, agents, auto_spawn list
//! - `workflow.md` — human-readable routing prose, surfaced by
//!   `pm workflow show`
//!
//! The TOML schema is intentionally minimal — v1 only uses `description`,
//! `agents`, and `auto_spawn`. New fields can be added later without
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
    /// All agents involved in the workflow.
    ///
    /// TODO: this is documentary-only in v1 — pm does not validate or
    /// enforce it against `auto_spawn` or installed agent definitions.
    /// A future pass could cross-check (every name in `auto_spawn`
    /// must be in `agents`; every name in `agents` should have an
    /// installed definition) so users get an early signal when the
    /// workflow drifts from reality.
    #[serde(default)]
    pub agents: Vec<String>,
    /// Agents to spawn at `pm feat new --workflow <name>` time. Each
    /// receives a copy of the `--context` message. Empty is valid (the
    /// workflow stores routing but never auto-spawns).
    #[serde(default)]
    pub auto_spawn: Vec<String>,
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

    /// Validate that every agent listed in `auto_spawn` has a definition
    /// file resolvable from the main worktree or the global agents
    /// directory. The feature worktree typically doesn't exist yet when
    /// this runs, so it isn't consulted.
    pub fn validate_auto_spawn(&self, project_root: &Path, workflow_name: &str) -> Result<()> {
        self.validate_auto_spawn_with_home(project_root, workflow_name, dirs::home_dir().as_deref())
    }

    /// Test-friendly variant of [`validate_auto_spawn`] that takes an
    /// explicit home directory instead of reading `$HOME` from the
    /// process environment. Production callers should use
    /// [`validate_auto_spawn`]; tests use this to avoid races on
    /// process-global `$HOME`.
    pub fn validate_auto_spawn_with_home(
        &self,
        project_root: &Path,
        workflow_name: &str,
        home: Option<&Path>,
    ) -> Result<()> {
        for agent in &self.auto_spawn {
            if !definition_exists(project_root, agent, home) {
                let main_def = paths::main_worktree(project_root)
                    .join(".claude/agents")
                    .join(format!("{agent}.md"));
                let global_def = home
                    .map(|h| h.join(".claude/agents").join(format!("{agent}.md")))
                    .unwrap_or_else(|| PathBuf::from("~/.claude/agents/<name>.md"));
                return Err(PmError::WorkflowAgentMissing {
                    workflow: workflow_name.to_string(),
                    agent: agent.clone(),
                    main_def,
                    global_def,
                });
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

/// True iff an agent definition file is resolvable from the main worktree
/// or the supplied home directory. The feature worktree is intentionally
/// not consulted — at `feat new` time it doesn't exist yet.
fn definition_exists(project_root: &Path, agent: &str, home: Option<&Path>) -> bool {
    let filename = format!("{agent}.md");
    let main_def = paths::main_worktree(project_root)
        .join(".claude/agents")
        .join(&filename);
    if main_def.exists() {
        return true;
    }
    if let Some(home) = home {
        let global_def = home.join(".claude/agents").join(&filename);
        if global_def.exists() {
            return true;
        }
    }
    false
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
auto_spawn = ["a"]
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        assert_eq!(def.description, "x");
        assert_eq!(def.agents, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(def.auto_spawn, vec!["a".to_string()]);
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
        assert!(def.auto_spawn.is_empty());
    }

    #[test]
    fn load_missing_workflow_returns_workflow_not_found() {
        let dir = tempdir().unwrap();
        let err = WorkflowDef::load(dir.path(), "missing").unwrap_err();
        assert!(matches!(err, PmError::WorkflowNotFound(_)));
    }

    #[test]
    fn validate_auto_spawn_ok_when_definition_in_main() {
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
        def.validate_auto_spawn(dir.path(), "demo").unwrap();
    }

    #[test]
    fn validate_auto_spawn_errors_when_missing() {
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
auto_spawn = ["frontend-impl"]
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        // Use the explicit-home variant so the test never mutates process
        // env. Pointing the home at our tempdir guarantees no spurious hit
        // on a user's real `~/.claude/agents/frontend-impl.md`.
        let result = def.validate_auto_spawn_with_home(dir.path(), "demo", Some(dir.path()));
        assert!(matches!(
            result.unwrap_err(),
            PmError::WorkflowAgentMissing { .. }
        ));
    }

    #[test]
    fn validate_auto_spawn_errors_when_home_is_none() {
        // When `home` is None, `validate_auto_spawn` should also fail
        // cleanly if main has no matching definition.
        let dir = tempdir().unwrap();
        write_workflow(
            dir.path(),
            "demo",
            r#"description = "x"
auto_spawn = ["frontend-impl"]
"#,
        );
        let def = WorkflowDef::load(dir.path(), "demo").unwrap();
        let result = def.validate_auto_spawn_with_home(dir.path(), "demo", None);
        assert!(matches!(
            result.unwrap_err(),
            PmError::WorkflowAgentMissing { .. }
        ));
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
