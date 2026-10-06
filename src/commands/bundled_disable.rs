//! The global config's `[bundled.disable]` table: bundled items the user keeps out of
//! the global tier. A disabled item is removed from the tier and its harness
//! projections at install time ([`skills::install_global_in`]) and skipped on
//! every later install; resolution stays disk-based, so a project custom of
//! the same name resolves as usual. Only where resolution then fails does a
//! disabled name change anything: the not-found error becomes
//! [`PmError::BundledDisabled`], naming the config key and the custom.
//!
//! A listed name pm doesn't bundle never matches; `pm doctor` reports it.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::paths;
use crate::state::project::{BundledDisable, GlobalConfig};
use crate::state::workflow;

use super::feat_common::DEFAULT_WORKFLOW;
use super::feat_review::REVIEW_WORKFLOW;

use super::skills::{self, BundledKind};

#[derive(Debug, Clone, Default)]
pub struct Disabled(BundledDisable);

impl Disabled {
    /// The `[bundled.disable]` table of the global config in `config_dir`. A
    /// malformed config is an error: an install must not quietly reinstall
    /// everything the user disabled.
    pub fn load_in(config_dir: &Path) -> Result<Self> {
        Ok(Self(GlobalConfig::load(config_dir)?.bundled.disable))
    }

    /// The real global config's table, empty when it can't be read. For
    /// refusal messages only, where the plain not-found error is the
    /// fallback.
    pub fn load() -> Self {
        Self(GlobalConfig::load_or_default().bundled.disable)
    }

    #[cfg(test)]
    pub fn from_config(config: BundledDisable) -> Self {
        Self(config)
    }

    pub(crate) fn contains(&self, kind: BundledKind, name: &str) -> bool {
        let listed = match kind {
            BundledKind::Skill => &self.0.skills,
            BundledKind::Agent => &self.0.agents,
            BundledKind::Workflow => &self.0.workflows,
            BundledKind::Baseline => return self.baseline(),
        };
        listed.iter().any(|n| n == name) && skills::is_bundled(kind, name)
    }

    pub fn agent(&self, name: &str) -> bool {
        self.contains(BundledKind::Agent, name)
    }

    pub fn workflow(&self, name: &str) -> bool {
        self.contains(BundledKind::Workflow, name)
    }

    pub fn baseline(&self) -> bool {
        self.0.baseline
    }

    /// Every disabled bundled item, as `Kind 'name'`.
    pub fn items(&self) -> Vec<String> {
        skills::bundled_items()
            .filter(|(kind, name)| self.contains(*kind, name))
            .map(|(kind, name)| format!("{} '{name}'", kind.label()))
            .collect()
    }

    /// Listed names pm doesn't bundle, as `key 'name'`.
    pub fn unknown(&self) -> Vec<String> {
        [
            ("agents", BundledKind::Agent, &self.0.agents),
            ("workflows", BundledKind::Workflow, &self.0.workflows),
            ("skills", BundledKind::Skill, &self.0.skills),
        ]
        .into_iter()
        .flat_map(|(key, kind, names)| {
            names
                .iter()
                .filter(move |n| !skills::is_bundled(kind, n))
                .map(move |n| format!("{key} '{n}'"))
        })
        .collect()
    }

    /// What pm or a workflow needs by a name this table disables and no
    /// custom resolves, one line per reference: the agent teams of installed
    /// workflows, the `solo` default, `pm feat review`'s workflow and agent,
    /// the `main` orchestrator, and the baseline.
    pub fn dangling(
        &self,
        project_root: &Path,
        global_dir: &Path,
        home: Option<&Path>,
    ) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let agent_missing =
            |name: &str| self.agent(name) && !workflow::definition_exists(project_root, name, home);
        let workflow_missing = |name: &str| {
            self.workflow(name)
                && workflow::resolve_dir(Some(project_root), name, global_dir).is_none()
        };

        for w in workflow::list_installed_in(Some(project_root), global_dir)?.workflows {
            for agent in w.def.effective_team().iter().filter(|a| agent_missing(a)) {
                out.push(format!("workflow '{}' needs agent '{agent}'", w.name));
            }
        }
        if workflow_missing(DEFAULT_WORKFLOW) {
            out.push(format!(
                "`pm feat new --context` without `--workflow` needs workflow '{DEFAULT_WORKFLOW}'"
            ));
        }
        if workflow_missing(REVIEW_WORKFLOW) {
            out.push(format!(
                "`pm feat review` needs workflow '{REVIEW_WORKFLOW}'"
            ));
        }
        if agent_missing("reviewer") {
            out.push("`pm feat review` needs agent 'reviewer'".to_string());
        }
        if agent_missing("main") {
            out.push("the orchestrator `main` needs agent 'main'".to_string());
        }
        if self.baseline() {
            out.push(
                "with no baseline, agents aren't told to run `pm workflow show` or how to use \
                 `pm msg`"
                    .to_string(),
            );
        }
        Ok(out)
    }

    /// `err` as [`PmError::BundledDisabled`] when it is a failed resolution of
    /// an agent definition or workflow this table disables, naming where a
    /// project custom of that name would go; otherwise `err` unchanged.
    pub fn explain(&self, project_root: &Path, err: PmError) -> PmError {
        let (label, key, name, custom) = match &err {
            PmError::WorkflowNotFound(name) if self.workflow(name) => (
                "Workflow",
                "workflows",
                name,
                paths::workflows_dir(project_root)
                    .join(name)
                    .join("config.toml"),
            ),
            PmError::WorkflowAgentMissing { agent, .. }
            | PmError::AgentDefinitionMissing { agent, .. }
                if self.agent(agent) =>
            {
                (
                    "Agent definition",
                    "agents",
                    agent,
                    paths::main_worktree(project_root)
                        .join(".agents/agents")
                        .join(format!("{agent}.md")),
                )
            }
            _ => return err,
        };
        PmError::BundledDisabled {
            kind: label,
            key,
            name: name.clone(),
            custom: custom.display().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disabled(toml: &str) -> Disabled {
        Disabled::from_config(toml::from_str(toml).unwrap())
    }

    #[test]
    fn only_bundled_names_match_and_the_rest_are_unknown() {
        let d = disabled(
            "agents = [\"qa\", \"planner\"]\nworkflows = [\"solo\"]\nskills = [\"pm\", \"qa\"]\n\
             baseline = true\n",
        );
        assert!(d.agent("qa"));
        assert!(!d.agent("planner"));
        assert!(d.workflow("solo"));
        assert!(!d.workflow("qa"));
        assert!(d.baseline());
        assert_eq!(d.unknown(), vec!["agents 'planner'", "skills 'qa'"]);
        assert_eq!(
            d.items(),
            vec![
                "Skill 'pm'",
                "Agent 'qa'",
                "Baseline 'pm-baseline'",
                "Workflow 'solo'"
            ]
        );
        assert!(!disabled("baseline = false\n").baseline());
        assert!(!Disabled::default().baseline());
    }

    #[test]
    fn explain_rewrites_only_a_disabled_names_not_found() {
        let root = Path::new("/p");
        let d = disabled("agents = [\"qa\"]\nworkflows = [\"solo\"]\n");
        let missing = |agent: &str| PmError::AgentDefinitionMissing {
            agent: agent.into(),
            searched: vec![],
        };
        let err = d.explain(root, missing("qa"));
        assert!(
            matches!(&err, PmError::BundledDisabled { key: "agents", name, .. } if name == "qa"),
            "{err}"
        );
        assert!(
            err.to_string().contains("/p/main/.agents/agents/qa.md"),
            "{err}"
        );
        assert!(matches!(
            d.explain(root, missing("reviewer")),
            PmError::AgentDefinitionMissing { .. }
        ));
        assert!(matches!(
            d.explain(root, PmError::WorkflowNotFound("solo".into())),
            PmError::BundledDisabled {
                key: "workflows",
                ..
            }
        ));
        assert!(matches!(
            d.explain(root, PmError::WorkflowNotFound("qa".into())),
            PmError::WorkflowNotFound(_)
        ));
    }
}
