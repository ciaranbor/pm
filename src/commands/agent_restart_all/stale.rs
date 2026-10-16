//! Which agents [`Select::Stale`](super::Select::Stale) restarts: running
//! ones whose launch is stale ([`launch_stamp`]). A dead agent, or one of a
//! closed session, launches with what is current at its next
//! spawn anyway. The caller, which asked for no restart of its own, and an
//! agent mid-turn are deferred: each restarts at its next idle
//! ([`restart_at_idle`](crate::commands::restart_at_idle)), unless `[upgrade]
//! restart_agents = false`, which leaves them reported as skipped. A
//! stale agent whose configured harness changed, which a restart would
//! start over on a new conversation, and one that would not relaunch
//! (`StaleCheck::unlaunchable`) are left running, each reported with the
//! command that restarts it.

use crate::commands::attention::AgentState;
use crate::commands::{agent_spawn, harness_check, launch_stamp};
use crate::error::{PmError, Result};
use crate::harness::Harness;
use crate::state::agent::{AgentEntry, AgentRegistry};
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig};

use super::{Scope, restart_command};

/// What [`Select::Stale`](super::Select::Stale) makes of one agent.
pub(in crate::commands) enum Verdict {
    /// Not running: its next spawn launches with what is current.
    Unrun,
    UpToDate,
    /// Stale, but left for the user to restart, for the reason given.
    Held(String),
    /// Whether it is stale could not be told, for the reason given.
    Unknown(String),
    /// Stale, and runs this command.
    Caller,
    Stale,
}

/// What [`Select::Stale`](super::Select::Stale) reads once per scope.
pub(in crate::commands) struct StaleCheck {
    registry: AgentRegistry,
    config: ProjectConfig,
    global: GlobalConfig,
}

impl StaleCheck {
    pub(in crate::commands) fn read(scope: &Scope, global: &GlobalConfig) -> Result<Self> {
        Ok(Self {
            registry: AgentRegistry::load(&paths::agents_dir(&scope.root), &scope.name)?,
            config: ProjectConfig::load(&paths::pm_dir(&scope.root))?,
            global: global.clone(),
        })
    }

    /// The harness `agent` runs on.
    pub(in crate::commands) fn harness(&self, agent: &str) -> Result<Harness> {
        Ok(self.entry(agent)?.harness)
    }

    fn entry(&self, agent: &str) -> Result<&AgentEntry> {
        self.registry
            .get(agent)
            .ok_or_else(|| PmError::Agent(format!("agent '{agent}' is not registered")))
    }

    /// The caller asked for no restart of its own, and a restart onto
    /// another harness would start its conversation over.
    pub(in crate::commands) fn verdict(
        &self,
        scope: &Scope,
        agent: &str,
        state: AgentState,
        is_caller: bool,
    ) -> Verdict {
        let unrun = match state {
            AgentState::Stopped | AgentState::Closed => true,
            AgentState::Dead => !is_caller,
            _ => false,
        };
        if unrun {
            return Verdict::Unrun;
        }
        let unknown = |e: PmError| {
            Verdict::Unknown(format!(
                "Failed to tell whether agent '{agent}' is stale: {e}"
            ))
        };
        let entry = match self.entry(agent) {
            Ok(entry) => entry,
            Err(e) => return unknown(e),
        };
        let (root, name) = (&scope.root, &scope.name);
        match launch_stamp::is_stale(root, name, agent, entry, &self.config, &self.global) {
            Ok(true) => {}
            Ok(false) => return Verdict::UpToDate,
            Err(e) => return unknown(e),
        }
        let restart = restart_command(scope, agent);
        let configured = agent_spawn::configured_harness(
            entry.effective_definition(agent),
            &self.config.agents,
            &self.global.agents,
        );
        match configured {
            Ok(harness) if harness != entry.harness => Verdict::Held(format!(
                "it is configured for {harness} now, so a restart starts a fresh conversation \
                 instead of resuming its {} one; {restart} does that",
                entry.harness
            )),
            Ok(_) if is_caller => Verdict::Caller,
            Ok(_) => Verdict::Stale,
            Err(e) => unknown(e),
        }
    }

    /// Each of `agents` that would not relaunch, with why: what
    /// [`harness_check::launch_problems`] or the spawn's own definition check
    /// refuses.
    pub(in crate::commands) fn unlaunchable(
        &self,
        scope: &Scope,
        agents: &[String],
    ) -> Result<Vec<(String, String)>> {
        let mut definitions = Vec::new();
        for agent in agents {
            definitions.push(self.entry(agent)?.effective_definition(agent).to_string());
        }
        let mut unique = definitions.clone();
        unique.sort();
        unique.dedup();
        let mut problems =
            harness_check::launch_problems(&scope.root, &self.config, &self.global, &unique)?;
        for definition in &unique {
            if let Err(e) = agent_spawn::validate_definition_resolves(&scope.root, definition) {
                problems.push((definition.clone(), e.to_string()));
            }
        }
        Ok(agents
            .iter()
            .zip(&definitions)
            .filter_map(|(agent, definition)| {
                let lines: Vec<&str> = problems
                    .iter()
                    .filter(|(d, _)| d == definition)
                    .map(|(_, line)| line.as_str())
                    .collect();
                (!lines.is_empty()).then(|| (agent.clone(), lines.join("; ")))
            })
            .collect())
    }
}
