//! A project's agents: those with a live window, which must stop before
//! the move, and the harnesses active ones run on.

use std::path::Path;

use crate::commands::running_agents::{self, Windows};
use crate::harness::Harness;
use crate::state::agent::AgentRegistry;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;

pub(super) struct Agents {
    /// `scope/name` of each agent with a live window.
    pub running: Vec<String>,
    /// The harnesses active agents run on.
    pub harnesses: Vec<Harness>,
}

impl Agents {
    pub(super) fn read(
        root: &Path,
        name: &str,
        pm_dir: &Path,
        features: &[(String, FeatureState)],
        windows: Option<&Windows>,
    ) -> Self {
        let project_name =
            ProjectConfig::load(pm_dir).map_or(name.to_string(), |c| c.project.name.clone());
        let mut scopes = vec!["main".to_string()];
        scopes.extend(
            features
                .iter()
                .filter(|(_, f)| f.status.is_active())
                .map(|(n, _)| n.clone()),
        );
        let mut out = Self {
            running: Vec::new(),
            harnesses: Vec::new(),
        };
        for scope in &scopes {
            if let Some(windows) = windows {
                out.running.extend(
                    running_agents::running_in_scope(root, &project_name, scope, windows)
                        .into_iter()
                        .map(|a| format!("{scope}/{}", a.name)),
                );
            }
            if let Ok(registry) = AgentRegistry::load(&paths::agents_dir(root), scope) {
                for entry in registry.agents.values().filter(|e| e.active) {
                    if !out.harnesses.contains(&entry.harness) {
                        out.harnesses.push(entry.harness);
                    }
                }
            }
        }
        out
    }
}
