//! A project's scopes: `main` and each feature.
//!
//! Every scope has a team status ([`TeamStatus`]). A feature's lives in its
//! state file; main's in `.pm/main.toml`, kept out of `features/` so main
//! never lists as a feature. A missing `main.toml` is a working main.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::state::agent::AgentRegistry;
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;
use crate::state::workflow::WorkflowDef;

/// The orchestrator's scope.
pub const MAIN: &str = "main";

/// The workflow main's scope runs; a project shadows it like any bundled one.
pub const MAIN_WORKFLOW: &str = "orchestrator";

/// The workflow `scope` runs: [`MAIN_WORKFLOW`] for main, else the one the
/// feature was created with, if any.
pub fn workflow(project_root: &Path, scope: &str) -> Result<Option<String>> {
    if scope == MAIN {
        return Ok(Some(MAIN_WORKFLOW.to_string()));
    }
    Ok(FeatureState::load(&paths::features_dir(project_root), scope)?.workflow)
}

/// The bundled orchestrator definition: what [`orchestrator_definition`]
/// falls back to when main's workflow can't be read.
pub const DEFAULT_ORCHESTRATOR: &str = "main";

/// The definition main's orchestrator runs: the lead (first) of
/// [`MAIN_WORKFLOW`]'s team, so a project that shadows the workflow with
/// another lead gets it everywhere pm treats the orchestrator specially.
pub fn orchestrator_definition(project_root: &Path) -> String {
    WorkflowDef::load(project_root, MAIN_WORKFLOW)
        .ok()
        .and_then(|def| def.effective_team().first().cloned())
        .unwrap_or_else(|| DEFAULT_ORCHESTRATOR.to_string())
}

/// Where a scope's team stands, set by its agents with `pm feat status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamStatus {
    #[serde(default)]
    pub progress: Progress,
    /// What a blocked team is waiting on the user for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    /// The agent that marked the scope blocked: the one to answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
}

impl TeamStatus {
    /// `scope`'s status; a feature that doesn't exist is an error.
    pub fn load(project_root: &Path, scope: &str) -> Result<Self> {
        if scope != MAIN {
            return Ok(FeatureState::load(&paths::features_dir(project_root), scope)?.team);
        }
        match std::fs::read_to_string(main_status_path(project_root)) {
            Ok(content) => Ok(toml::from_str(&content)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Record this as `scope`'s status. A feature's last activity is
    /// stamped with it.
    pub fn save(&self, project_root: &Path, scope: &str) -> Result<()> {
        if scope != MAIN {
            let features_dir = paths::features_dir(project_root);
            let mut state = FeatureState::load(&features_dir, scope)?;
            state.team = self.clone();
            state.last_active = chrono::Utc::now();
            return state.save(&features_dir, scope);
        }
        crate::fs_utils::write_atomic(
            &main_status_path(project_root),
            toml::to_string_pretty(self)?.as_bytes(),
        )
    }

    /// The reason, only while blocked.
    pub fn reason(&self) -> Option<&str> {
        self.blocked_reason
            .as_deref()
            .filter(|_| self.progress == Progress::Blocked)
    }

    /// The agent that blocked it, only while blocked.
    pub fn agent(&self) -> Option<&str> {
        self.blocked_by
            .as_deref()
            .filter(|_| self.progress == Progress::Blocked)
    }
}

/// The agent in main's registry that runs [`orchestrator_definition`]: the
/// first active one by name, else the first inactive one; while none does,
/// the definition's name, which is what `pm init` spawns it as.
pub fn orchestrator(project_root: &Path) -> String {
    let definition = orchestrator_definition(project_root);
    let Ok(registry) = AgentRegistry::load(&paths::agents_dir(project_root), MAIN) else {
        return definition;
    };
    let mut runs_it = registry
        .agents
        .iter()
        .filter(|(name, entry)| entry.effective_definition(name) == definition);
    let first = runs_it.clone().next();
    runs_it
        .find(|(_, entry)| entry.active)
        .or(first)
        .map_or(definition.clone(), |(name, _)| name.clone())
}

/// Main's team status: `<project>/.pm/main.toml`.
fn main_status_path(project_root: &Path) -> PathBuf {
    paths::pm_dir(project_root).join("main.toml")
}

/// Every scope of the project at `project_root`: main, then each feature.
pub fn names(project_root: &Path) -> Result<Vec<String>> {
    let features = FeatureState::list(&paths::features_dir(project_root))?;
    Ok(std::iter::once(MAIN.to_string())
        .chain(features.into_iter().map(|(name, _)| name))
        .collect())
}

/// `scope`'s worktree while its team runs: main's always, a feature's
/// while its status is active; `None` otherwise.
pub fn active_worktree(project_root: &Path, scope: &str) -> Result<Option<PathBuf>> {
    if scope == MAIN {
        return Ok(Some(paths::main_worktree(project_root)));
    }
    let state = FeatureState::load(&paths::features_dir(project_root), scope)?;
    Ok(state
        .status
        .is_active()
        .then(|| project_root.join(&state.worktree)))
}

/// Whether `scope` is main or a known feature.
pub fn exists(project_root: &Path, scope: &str) -> bool {
    scope == MAIN || FeatureState::exists(&paths::features_dir(project_root), scope)
}

/// `Ok` if `scope` is main or a known feature.
pub fn check(project_root: &Path, scope: &str) -> Result<()> {
    if exists(project_root, scope) {
        Ok(())
    } else {
        Err(PmError::FeatureNotFound(scope.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn the_orchestrator_is_mains_agent_running_its_definition() {
        let dir = tempdir().unwrap();
        assert_eq!(orchestrator(dir.path()), DEFAULT_ORCHESTRATOR);

        let agents_dir = paths::agents_dir(dir.path());
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("main.toml"),
            "[agents.a-old]\ntype = \"agent\"\nactive = false\nagent_definition = \"main\"\n\n\
             [agents.helper]\ntype = \"agent\"\nagent_definition = \"plain\"\n\n\
             [agents.orch]\ntype = \"agent\"\nagent_definition = \"main\"\n",
        )
        .unwrap();
        assert_eq!(orchestrator(dir.path()), "orch");

        std::fs::write(
            agents_dir.join("main.toml"),
            "[agents.a-old]\ntype = \"agent\"\nactive = false\nagent_definition = \"main\"\n",
        )
        .unwrap();
        assert_eq!(orchestrator(dir.path()), "a-old");
    }

    #[test]
    fn a_project_workflow_names_its_own_orchestrator() {
        let dir = tempdir().unwrap();
        let workflow = paths::workflows_dir(dir.path()).join(MAIN_WORKFLOW);
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::write(
            workflow.join("config.toml"),
            "description = \"d\"\nagents = [\"lead\", \"scribe\"]\n",
        )
        .unwrap();
        assert_eq!(orchestrator_definition(dir.path()), "lead");

        let agents_dir = paths::agents_dir(dir.path());
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("main.toml"),
            "[agents.main]\ntype = \"agent\"\n\n\
             [agents.boss]\ntype = \"agent\"\nagent_definition = \"lead\"\n",
        )
        .unwrap();
        assert_eq!(orchestrator(dir.path()), "boss");
    }

    #[test]
    fn mains_status_is_wip_until_saved_and_round_trips() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(paths::pm_dir(dir.path())).unwrap();
        assert_eq!(
            TeamStatus::load(dir.path(), MAIN).unwrap(),
            TeamStatus::default()
        );

        let blocked = TeamStatus {
            progress: Progress::Blocked,
            blocked_reason: Some("next item?".into()),
            blocked_by: Some("main".into()),
        };
        blocked.save(dir.path(), MAIN).unwrap();
        assert_eq!(TeamStatus::load(dir.path(), MAIN).unwrap(), blocked);
        assert!(
            FeatureState::list(&paths::features_dir(dir.path()))
                .unwrap()
                .is_empty(),
            "main must never list as a feature"
        );
    }

    #[test]
    fn a_scope_is_main_or_a_known_feature() {
        let dir = tempdir().unwrap();
        let features_dir = paths::features_dir(dir.path());
        std::fs::create_dir_all(&features_dir).unwrap();
        std::fs::write(features_dir.join("login.toml"), "").unwrap();

        assert!(check(dir.path(), MAIN).is_ok());
        assert!(check(dir.path(), "login").is_ok());
        assert!(matches!(
            check(dir.path(), "bogus").unwrap_err(),
            PmError::FeatureNotFound(n) if n == "bogus"
        ));
    }

    #[test]
    fn a_missing_features_status_is_an_error() {
        let dir = tempdir().unwrap();
        let err = TeamStatus::load(dir.path(), "login").unwrap_err();
        assert!(matches!(err, PmError::FeatureNotFound(n) if n == "login"));
    }
}
