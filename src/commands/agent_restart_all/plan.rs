//! What a sweep would do, decided from one scan of the server: which
//! agents restart now, which at their next idle, and which not at all.

use crate::commands::agent_restart::callers_agent;
use crate::commands::attention::{AgentState, scope_agents_in};
use crate::commands::running_agents::Windows;
use crate::error::{PmError, Result};
use crate::state::project::GlobalConfig;

use super::{Outcome, Report, Scope, StaleCheck, Verdict};

/// Which agents a sweep restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    /// Every active agent.
    All,
    /// The stale ones ([`launch_stamp`](crate::commands::launch_stamp))
    /// that are running; the caller and those mid-turn are deferred
    /// ([`super::stale`]).
    Stale,
}

impl Select {
    /// How to restart an agent skipped as mid-turn.
    pub(super) fn hint(self, scope: &Scope, agent: &str) -> String {
        match self {
            Select::All => "--force interrupts it and has it resume".to_string(),
            Select::Stale => format!(
                "restart it once idle with {}",
                restart_command(scope, agent)
            ),
        }
    }
}

/// What a sweep would do, read from one scan of the server.
#[derive(Debug, Default)]
pub struct Plan {
    /// The agents and scopes settled without a restart.
    pub unrestarted: Vec<Report>,
    /// Each scope's agents to restart, and whether the caller is among them.
    pub planned: Vec<(Scope, Vec<String>, bool)>,
    /// The agents to restart at their next idle, each with why not now.
    pub deferred: Vec<(Scope, String, String)>,
    /// How many [`Select::Stale`] passed over as up to date.
    pub up_to_date: usize,
    /// How many running agents [`Select::Stale`] found stale.
    pub stale: usize,
}

impl Plan {
    /// Take each planned agent `why` gives a reason for out of the plan,
    /// reporting it skipped for that reason with the command that restarts
    /// it.
    pub fn hold(&mut self, why: impl Fn(&Scope, &str) -> Option<String>) {
        for (scope, names, _) in &mut self.planned {
            names.retain(|name| match why(scope, name) {
                Some(why) => {
                    self.unrestarted.push(held_back(scope, name, &why));
                    false
                }
                None => true,
            });
        }
        self.planned.retain(|(_, names, _)| !names.is_empty());
    }

    /// One `Would restart` line per planned or deferred agent.
    pub fn would_restart(&self) -> Vec<String> {
        let now = self.planned.iter().flat_map(|(scope, names, _)| {
            names
                .iter()
                .map(move |name| format!("{}: Would restart agent '{name}'", scope.label()))
        });
        let deferred = self.deferred.iter().map(|(scope, name, why)| {
            format!(
                "{}: Would restart agent '{name}' at its next idle ({why})",
                scope.label()
            )
        });
        now.chain(deferred).collect()
    }
}

/// Plan a sweep of `scopes` ([`super`]'s docs).
pub fn plan(
    scopes: &[Scope],
    select: Select,
    force: bool,
    tmux_server: Option<&str>,
) -> Result<Plan> {
    let windows = Windows::read(tmux_server)?;
    let global = (select == Select::Stale).then(GlobalConfig::load_or_default);
    let defer = global.as_ref().is_some_and(|g| g.upgrade.restarts_agents());
    let mut plan = Plan::default();
    for scope in scopes {
        let report = |outcome: Outcome| Report {
            target: scope.label(),
            outcome,
        };
        let failed =
            |e: PmError| report(Outcome::Failed(format!("Failed to read its agents: {e}")));
        let agents = match scope_agents_in(&scope.root, &scope.name, &windows) {
            Ok(agents) => agents,
            Err(e) => {
                plan.unrestarted.push(failed(e));
                continue;
            }
        };
        let stale = match &global {
            None => None,
            Some(global) => match StaleCheck::read(scope, global) {
                Ok(check) => Some(check),
                Err(e) => {
                    plan.unrestarted.push(failed(e));
                    continue;
                }
            },
        };
        let active: Vec<String> = agents
            .iter()
            .filter(|a| a.state != AgentState::Stopped)
            .map(|a| a.name.clone())
            .collect();
        let caller = callers_agent(&scope.root, &scope.name, &active, tmux_server);
        let mut names = Vec::new();
        for agent in agents {
            let skip = |why: &str| {
                report(Outcome::Skipped(format!(
                    "Skipped agent '{}': {why}",
                    agent.name
                )))
            };
            let is_caller = Some(&agent.name) == caller;
            if let Some(check) = &stale {
                let verdict = check.verdict(scope, &agent.name, agent.state, is_caller);
                if matches!(verdict, Verdict::Stale | Verdict::Held(_) | Verdict::Caller) {
                    plan.stale += 1;
                }
                match verdict {
                    Verdict::Unrun => continue,
                    Verdict::UpToDate => {
                        plan.up_to_date += 1;
                        continue;
                    }
                    Verdict::Held(why) => {
                        plan.unrestarted.push(skip(&why));
                        continue;
                    }
                    Verdict::Caller if defer => {
                        plan.deferred.push((
                            scope.clone(),
                            agent.name,
                            "it runs this command".to_string(),
                        ));
                        continue;
                    }
                    Verdict::Caller => {
                        plan.unrestarted.push(skip(&format!(
                            "it is stale, and runs this command; restart it with {}",
                            restart_command(scope, &agent.name)
                        )));
                        continue;
                    }
                    Verdict::Unknown(why) => {
                        plan.unrestarted.push(report(Outcome::Failed(why)));
                        continue;
                    }
                    Verdict::Stale => {}
                }
            }
            match agent.state {
                AgentState::Stopped => {}
                AgentState::Closed => plan.unrestarted.push(skip("its session is closed")),
                _ if is_caller => names.push(agent.name),
                AgentState::Busy | AgentState::Asking | AgentState::Background
                    if !force && defer =>
                {
                    let why = mid_turn(agent.state).to_string();
                    plan.deferred.push((scope.clone(), agent.name, why));
                }
                AgentState::Busy | AgentState::Asking | AgentState::Background if !force => {
                    let hint = select.hint(scope, &agent.name);
                    plan.unrestarted
                        .push(skip(&format!("{}; {hint}", mid_turn(agent.state))));
                }
                _ => names.push(agent.name),
            }
        }
        if let Some(check) = &stale
            && !names.is_empty()
        {
            match check.unlaunchable(scope, &names) {
                Ok(unlaunchable) => {
                    for (agent, why) in unlaunchable {
                        names.retain(|n| *n != agent);
                        plan.unrestarted.push(report(Outcome::Skipped(format!(
                            "Skipped agent '{agent}': it is stale, but would not relaunch \
                             ({why}); fix that, then {}",
                            restart_command(scope, &agent)
                        ))));
                    }
                }
                Err(e) => {
                    for agent in names.drain(..) {
                        plan.unrestarted.push(report(Outcome::Failed(format!(
                            "Failed to tell whether agent '{agent}' would relaunch: {e}"
                        ))));
                    }
                }
            }
        }
        if !names.is_empty() {
            let has_caller = caller.is_some_and(|c| names.contains(c));
            plan.planned.push((scope.clone(), names, has_caller));
        }
    }
    plan.planned.sort_by_key(|(.., caller)| *caller);
    Ok(plan)
}

/// `agent` of `scope` skipped, held back for `why` until it clears.
pub(super) fn held_back(scope: &Scope, agent: &str, why: &str) -> Report {
    Report {
        target: scope.label(),
        outcome: Outcome::Skipped(format!(
            "Skipped agent '{agent}': {why}; restart it once that clears with {}",
            restart_command(scope, agent)
        )),
    }
}

/// The command that restarts `agent` of `scope`, quoted for a report line.
pub(super) fn restart_command(scope: &Scope, agent: &str) -> String {
    format!("`pm agent restart {agent} --scope {}`", scope.name)
}

/// Why restarting an agent in `state` would cut its turn short.
fn mid_turn(state: AgentState) -> &'static str {
    match state {
        AgentState::Asking => "a dialog waits on the user",
        AgentState::Background => "it is waiting on background work",
        _ => "it is mid-turn",
    }
}
