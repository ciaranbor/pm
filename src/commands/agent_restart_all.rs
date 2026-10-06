//! `pm agent restart --all`: restart every active agent of a scope, or of
//! every scope of every registered project, through the same per-scope
//! restart as `pm agent restart <names>` ([`agent_restart_many`]).
//!
//! Which agents to restart is decided from one scan of the server
//! ([`scope_agents_in`]); each scope's restart reads its agents again, so
//! one that turned mid-turn since is still refused. Idle, unarmed and dead
//! agents restart — a dead one is healed by the respawn. An agent mid-turn,
//! asking, or running background work is skipped unless `force`, which
//! interrupts it and has it resume. An agent whose scope's session is
//! closed — a closed feature, or a project not opened — is skipped:
//! restarting it would open the session. Stopped agents aren't active and
//! aren't listed. The agent this process runs in is never skipped as busy
//! — it asked for the restart — and its scope goes last, so its old pane,
//! which ends this process when killed, outlives every other restart
//! ([`RestartAll::finish`]). Window focus is left alone: a sweep over many
//! scopes must not move the user's clients from window to window.
//!
//! [`Select::Stale`] (`--stale`, and `pm upgrade`) narrows the sweep to
//! running agents whose launch is stale ([`launch_stamp`]): a dead agent,
//! or one of a closed session, launches with what is current at its next
//! spawn anyway, and the caller, which asked for no restart of its own, is
//! reported instead.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::state::agent::{AgentEntry, AgentRegistry};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig, ProjectEntry};
use crate::tmux;

use super::agent_restart::{Restarted, agent_restart_many, callers_agent};
use super::attention::{AgentState, scope_agents_in};
use super::launch_check::{self, Launch};
use super::launch_stamp;
use super::running_agents::Windows;
use super::{agent_spawn, harness_check};

/// A scope of a project.
#[derive(Debug, Clone)]
pub struct Scope {
    pub root: PathBuf,
    /// The project's name, as its sessions are prefixed.
    pub project: String,
    pub name: String,
}

impl Scope {
    /// `scope` of the project at `root`.
    pub fn of(root: &Path, scope: &str) -> Result<Self> {
        let config = ProjectConfig::load(&paths::pm_dir(root))?;
        Ok(Self {
            root: root.to_path_buf(),
            project: config.project.name,
            name: scope.to_string(),
        })
    }

    fn label(&self) -> String {
        tmux::session_name(&self.project, &self.name)
    }
}

/// Every scope of the project at `root`: main, then each feature.
pub fn project_scopes(root: &Path) -> Result<Vec<Scope>> {
    let main = Scope::of(root, "main")?;
    let features = FeatureState::list(&paths::features_dir(root))?;
    let mut scopes = vec![main.clone()];
    scopes.extend(features.into_iter().map(|(name, _)| Scope {
        name,
        ..main.clone()
    }));
    Ok(scopes)
}

/// Every scope of every project registered in `projects_dir`, and a
/// skipped report for each project that can't be read.
pub fn global_scopes(projects_dir: &Path) -> Result<(Vec<Scope>, Vec<Report>)> {
    let registry = ProjectEntry::scan(projects_dir)?;
    let mut scopes = Vec::new();
    let mut unread = Vec::new();
    for (name, entry) in registry.projects {
        match project_scopes(&entry.root_path()) {
            Ok(found) => scopes.extend(found),
            Err(e) => unread.push(Report {
                target: name,
                outcome: Outcome::Skipped(format!("Skipped: project unreadable ({e})")),
            }),
        }
    }
    unread.extend(registry.malformed.into_iter().map(|bad| Report {
        target: bad.name,
        outcome: Outcome::Skipped(format!(
            "Skipped: registry entry unreadable ({})",
            bad.error
        )),
    }));
    Ok((scopes, unread))
}

/// Each holds the report's line.
#[derive(Debug)]
pub enum Outcome {
    Restarted(String),
    Skipped(String),
    Failed(String),
}

/// What happened to one agent, or to a whole project or scope.
#[derive(Debug)]
pub struct Report {
    /// The scope's session name, or the project's name.
    pub target: String,
    pub outcome: Outcome,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Outcome::Restarted(line) | Outcome::Skipped(line) | Outcome::Failed(line)) =
            &self.outcome;
        write!(f, "{}: {line}", self.target)
    }
}

/// What a sweep did to each agent.
#[derive(Debug, Default)]
pub struct Sweep {
    /// What was settled before any restart ran, then the restarts in the
    /// order they ran, the caller's last.
    pub reports: Vec<Report>,
    /// How many restarted agents `force` interrupted mid-turn.
    pub interrupted: usize,
    /// How many [`Select::Stale`] passed over as up to date.
    pub up_to_date: usize,
}

impl Sweep {
    pub fn count(&self, kind: fn(&Outcome) -> bool) -> usize {
        self.reports.iter().filter(|r| kind(&r.outcome)).count()
    }

    /// `Restarted N, skipped N, failed N`, and how many were interrupted.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "Restarted {}, skipped {}, failed {}",
            self.count(|o| matches!(o, Outcome::Restarted(_))),
            self.count(|o| matches!(o, Outcome::Skipped(_))),
            self.count(|o| matches!(o, Outcome::Failed(_))),
        );
        if self.up_to_date > 0 {
            line.push_str(&format!(", {} up to date", self.up_to_date));
        }
        if self.interrupted > 0 {
            line.push_str(&format!(
                " ({} interrupted mid-turn by --force, each told to resume)",
                self.interrupted
            ));
        }
        line
    }
}

/// The restarts of one scope.
#[derive(Debug)]
struct Batch {
    scope: Scope,
    done: Restarted,
}

/// The restarts [`restart_all`] ran.
#[derive(Debug)]
pub struct RestartAll {
    /// The agents and scopes settled without a restart.
    unrestarted: Vec<Report>,
    batches: Vec<Batch>,
    select: Select,
    up_to_date: usize,
}

impl RestartAll {
    /// Turn the result of each restarted agent whose harness exited at
    /// launch into a failure saying why. Every launch is watched at once.
    pub fn confirm_launches(&mut self, tmux_server: Option<&str>) {
        let launches: Vec<Launch> = self
            .batches
            .iter()
            .flat_map(|b| {
                b.done.launched().map(|agent| Launch {
                    project_root: b.scope.root.clone(),
                    scope: b.scope.name.clone(),
                    agent: agent.to_string(),
                })
            })
            .collect();
        let mut failed = launch_check::confirm_all(&launches, tmux_server);
        for batch in &mut self.batches {
            let (mine, rest) = failed.into_iter().partition(|f| {
                f.launch.project_root == batch.scope.root && f.launch.scope == batch.scope.name
            });
            failed = rest;
            batch.done.record_failures(mine);
        }
    }

    /// What happened to each agent. A restart refused because its agent
    /// was mid-turn by the time it ran is a skip.
    pub fn sweep(&mut self) -> Sweep {
        let mut sweep = Sweep {
            reports: std::mem::take(&mut self.unrestarted),
            interrupted: 0,
            up_to_date: self.up_to_date,
        };
        for batch in &mut self.batches {
            let results = std::mem::take(&mut batch.done.results);
            for (agent, result) in batch.done.agents.iter().zip(results) {
                let refused = batch.done.refused.iter().find(|(a, _)| a == agent);
                let outcome = match (result, refused) {
                    (Ok(line), _) => {
                        sweep.interrupted += usize::from(batch.done.interrupted.contains(agent));
                        Outcome::Restarted(line)
                    }
                    (Err(_), Some((_, why))) => Outcome::Skipped(format!(
                        "Skipped agent '{agent}': {why}; {}",
                        self.select.hint(&batch.scope, agent)
                    )),
                    (Err(PmError::Agent(why)), None) => {
                        let why = why
                            .strip_prefix(&format!("agent '{agent}': "))
                            .unwrap_or(&why);
                        Outcome::Failed(format!("Failed to restart agent '{agent}': {why}"))
                    }
                    (Err(e), None) => {
                        Outcome::Failed(format!("Failed to restart agent '{agent}': {e}"))
                    }
                };
                sweep.reports.push(Report {
                    target: batch.scope.label(),
                    outcome,
                });
            }
        }
        sweep
    }

    /// Kill the old pane of the agent this process runs in, if it was
    /// restarted: that ends the process, so it comes after the reports
    /// are printed.
    pub fn finish(self, tmux_server: Option<&str>) {
        for batch in self.batches {
            batch.done.finish(tmux_server);
        }
    }
}

/// Which agents a sweep restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    /// Every active agent.
    All,
    /// The stale ones ([`launch_stamp`]) that are running; never the
    /// caller, which is reported instead.
    Stale,
}

impl Select {
    /// How to restart an agent skipped as mid-turn.
    fn hint(self, scope: &Scope, agent: &str) -> String {
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
    /// How many [`Select::Stale`] passed over as up to date.
    pub up_to_date: usize,
}

impl Plan {
    /// One `Would restart` line per planned agent.
    pub fn would_restart(&self) -> Vec<String> {
        self.planned
            .iter()
            .flat_map(|(scope, names, _)| {
                names
                    .iter()
                    .map(move |name| format!("{}: Would restart agent '{name}'", scope.label()))
            })
            .collect()
    }
}

/// Plan a sweep of `scopes` (module docs).
pub fn plan(
    scopes: &[Scope],
    select: Select,
    force: bool,
    tmux_server: Option<&str>,
) -> Result<Plan> {
    let windows = Windows::read(tmux_server)?;
    let global = (select == Select::Stale).then(GlobalConfig::load_or_default);
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
                match check.verdict(scope, &agent.name, agent.state, is_caller) {
                    Verdict::Unrun => continue,
                    Verdict::UpToDate => {
                        plan.up_to_date += 1;
                        continue;
                    }
                    Verdict::Held(why) => {
                        plan.unrestarted.push(skip(&why));
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

/// The command that restarts `agent` of `scope`, quoted for a report line.
fn restart_command(scope: &Scope, agent: &str) -> String {
    format!("`pm agent restart {agent} --scope {}`", scope.name)
}

/// What [`Select::Stale`] makes of one agent.
enum Verdict {
    /// Not running: its next spawn launches with what is current.
    Unrun,
    UpToDate,
    /// Stale, but left for the user to restart, for the reason given.
    Held(String),
    /// Whether it is stale could not be told, for the reason given.
    Unknown(String),
    Stale,
}

/// What [`Select::Stale`] reads once per scope.
struct StaleCheck {
    registry: AgentRegistry,
    config: ProjectConfig,
    global: GlobalConfig,
}

impl StaleCheck {
    fn read(scope: &Scope, global: &GlobalConfig) -> Result<Self> {
        Ok(Self {
            registry: AgentRegistry::load(&paths::agents_dir(&scope.root), &scope.name)?,
            config: ProjectConfig::load(&paths::pm_dir(&scope.root))?,
            global: global.clone(),
        })
    }

    fn entry(&self, agent: &str) -> Result<&AgentEntry> {
        self.registry
            .get(agent)
            .ok_or_else(|| PmError::Agent(format!("agent '{agent}' is not registered")))
    }

    /// The caller asked for no restart of its own, and a restart onto
    /// another harness would start its conversation over.
    fn verdict(&self, scope: &Scope, agent: &str, state: AgentState, is_caller: bool) -> Verdict {
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
        if is_caller {
            return Verdict::Held(format!(
                "it is stale, and runs this command; restart it with {restart}"
            ));
        }
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
            Ok(_) => Verdict::Stale,
            Err(e) => unknown(e),
        }
    }

    /// Each of `agents` that would not relaunch, with why: what
    /// [`harness_check::launch_problems`] or the spawn's own definition check
    /// refuses.
    fn unlaunchable(&self, scope: &Scope, agents: &[String]) -> Result<Vec<(String, String)>> {
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

/// Restart the agents of `scopes` that `select` picks (module docs).
pub fn restart_all(
    scopes: &[Scope],
    select: Select,
    force: bool,
    tmux_server: Option<&str>,
) -> Result<RestartAll> {
    let plan = plan(scopes, select, force, tmux_server)?;
    let batches = plan
        .planned
        .into_iter()
        .map(|(scope, names, _)| Batch {
            done: agent_restart_many(&scope.root, &scope.name, &names, force, false, tmux_server),
            scope,
        })
        .collect();
    Ok(RestartAll {
        unrestarted: plan.unrestarted,
        batches,
        select,
        up_to_date: plan.up_to_date,
    })
}

/// Why restarting an agent in `state` would cut its turn short.
fn mid_turn(state: AgentState) -> &'static str {
    match state {
        AgentState::Asking => "a dialog waits on the user",
        AgentState::Background => "it is waiting on background work",
        _ => "it is mid-turn",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn lines(sweep: &Sweep) -> Vec<String> {
        sweep.reports.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn all_restarts_idle_and_dead_agents_and_skips_one_mid_turn() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project, &session, "login", "implementer");
        server.spawn_idle_fake_agent(&project, &session, "login", "reviewer");
        server.spawn_dead_fake_agent(&project, &session, "login", "qa");

        let scope = Scope::of(&project, "login").unwrap();
        let sweep = restart_all(&[scope], Select::All, false, server.name())
            .unwrap()
            .sweep();

        let lines = lines(&sweep);
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert_eq!(
            lines[0],
            format!(
                "{session}: Skipped agent 'implementer': it is mid-turn; --force interrupts it \
                 and has it resume"
            )
        );
        for (line, agent) in lines[1..].iter().zip(["qa", "reviewer"]) {
            assert!(
                line.starts_with(&format!("{session}: Restarted agent '{agent}'")),
                "{line}"
            );
        }
        assert_eq!(sweep.summary(), "Restarted 2, skipped 1, failed 0");
        assert!(
            tmux::pane_processes(server.name(), &format!("{session}:implementer"))
                .unwrap()
                .iter()
                .any(|p| p.command.contains("999")),
            "a skipped agent keeps running"
        );
    }

    #[test]
    fn stale_restarts_only_running_stale_agents_and_says_how_to_restart_a_busy_one() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project, &session, "login", "reviewer");
        server.spawn_idle_fake_agent(&project, &session, "login", "current");
        server.spawn_fake_agent(&project, &session, "login", "implementer");
        server.spawn_dead_fake_agent(&project, &session, "login", "qa");
        for agent in ["reviewer", "implementer", "qa"] {
            crate::state::runtime::write_launch_stamp(&project, "login", agent, "old").unwrap();
        }
        let runs = |agent: &str, what: &str| {
            tmux::pane_processes(server.name(), &format!("{session}:{agent}"))
                .unwrap()
                .iter()
                .any(|p| p.command.contains(what))
        };

        let scope = Scope::of(&project, "login").unwrap();
        let mut done = restart_all(&[scope], Select::Stale, false, server.name()).unwrap();
        let sweep = done.sweep();

        let lines = lines(&sweep);
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert_eq!(
            lines[0],
            format!(
                "{session}: Skipped agent 'implementer': it is mid-turn; restart it once idle \
                 with `pm agent restart implementer --scope login`"
            )
        );
        assert!(
            lines[1].starts_with(&format!("{session}: Restarted agent 'reviewer'")),
            "{lines:#?}"
        );
        assert_eq!(
            sweep.summary(),
            "Restarted 1, skipped 1, failed 0, 1 up to date"
        );
        assert!(
            runs("current", "sleep 999"),
            "an up-to-date agent keeps running"
        );
        assert!(runs("implementer", "999"), "a busy one too");
        assert!(
            !runs("qa", "claude"),
            "a dead one is left to its next spawn"
        );
    }

    #[test]
    fn stale_leaves_running_an_agent_that_would_not_relaunch_or_would_lose_its_conversation() {
        use crate::harness::Harness;
        use crate::state::project::ProjectConfig;
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        let pm_dir = paths::pm_dir(&project);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config.agents.harness.insert("qa".into(), "opencode".into());
        config.save(&pm_dir).unwrap();
        server.spawn_idle_fake_agent(&project, &session, "login", "reviewer");
        server.spawn_idle_fake_agent(&project, &session, "login", "ghost");
        let shell =
            crate::testing::fake_harness_binary(Harness::OpenCode, std::path::Path::new("/bin/sh"));
        server.spawn_harness_agent(
            &project,
            &session,
            "login",
            "qa",
            Harness::OpenCode,
            &format!(
                "{} -c \"sh -c 'sleep 999; :' {} opencode; :\"",
                shell.display(),
                crate::commands::hooks_install::PM_HOOK_MARKER
            ),
            super::super::running_agents::Liveness::Idle,
        );
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".into(), "codex".into());
        config.save(&pm_dir).unwrap();
        for agent in ["reviewer", "qa", "ghost"] {
            crate::state::runtime::write_launch_stamp(&project, "login", agent, "old").unwrap();
        }

        let scope = Scope::of(&project, "login").unwrap();
        let sweep = restart_all(&[scope], Select::Stale, false, server.name())
            .unwrap()
            .sweep();

        let lines = lines(&sweep);
        assert_eq!(
            sweep.summary(),
            "Restarted 0, skipped 3, failed 0",
            "{lines:#?}"
        );
        assert!(
            lines.contains(&format!(
                "{session}: Skipped agent 'reviewer': it is configured for codex now, so a \
                 restart starts a fresh conversation instead of resuming its claude-code one; \
                 `pm agent restart reviewer --scope login` does that"
            )),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with(&format!(
                "{session}: Skipped agent 'qa': it is stale, but would not relaunch ("
            )) && l.contains("no [agents.models] row")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with(&format!(
                "{session}: Skipped agent 'ghost': it is stale, but would not relaunch ("
            )) && l.contains("No agent definition 'ghost' found")),
            "{lines:#?}"
        );
        for agent in ["reviewer", "qa", "ghost"] {
            assert!(
                tmux::pane_processes(server.name(), &format!("{session}:{agent}"))
                    .unwrap()
                    .iter()
                    .any(|p| p.command.contains("sleep 999")),
                "{agent} keeps running"
            );
        }
    }

    #[test]
    fn all_with_force_interrupts_a_mid_turn_agent_and_tells_only_it_to_resume() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project, &session, "login", "implementer");
        server.spawn_idle_fake_agent(&project, &session, "login", "reviewer");

        let scope = Scope::of(&project, "login").unwrap();
        let sweep = restart_all(&[scope], Select::All, true, server.name())
            .unwrap()
            .sweep();

        let lines = lines(&sweep);
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(
            lines[0].starts_with(&format!("{session}: Restarted agent 'implementer'"))
                && lines[0].ends_with("is told to resume"),
            "{lines:#?}"
        );
        assert!(!lines[1].contains("resume"), "{lines:#?}");
        assert_eq!(
            sweep.summary(),
            "Restarted 2, skipped 0, failed 0 (1 interrupted mid-turn by --force, each told to \
             resume)"
        );
        let told = |agent: &str| {
            crate::messages::list(
                &paths::messages_dir(&project),
                "login",
                agent,
                Some(crate::commands::agent_restart::RESUME_SENDER),
            )
            .unwrap()
            .len()
        };
        assert_eq!((told("implementer"), told("reviewer")), (1, 0));
    }

    #[test]
    fn global_covers_every_scope_of_every_project_and_leaves_closed_sessions_closed() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (first, first_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&first);
        let other_name = server.scope("other");
        let other = dir.path().join(&other_name);
        crate::commands::init::init(&other, &projects_dir, None, server.name()).unwrap();
        let login = tmux::session_name(&first_name, "login");
        let other_main = tmux::session_name(&other_name, "main");
        server.spawn_idle_fake_agent(&first, &login, "login", "reviewer");
        server.spawn_idle_fake_agent(&other, &other_main, "main", "implementer");
        crate::commands::feat_new::feat_new(
            &crate::commands::feat_new::FeatNewParams::with_defaults(
                &first,
                &projects_dir,
                "signup",
                server.name(),
            ),
        )
        .unwrap();
        let signup = tmux::session_name(&first_name, "signup");
        server.spawn_dead_fake_agent(&first, &signup, "signup", "implementer");
        tmux::kill_session(server.name(), &signup).unwrap();
        let first_main = tmux::session_name(&first_name, "main");
        server.spawn_dead_fake_agent(&first, &first_main, "main", "qa");
        tmux::kill_session(server.name(), &first_main).unwrap();

        let (scopes, unread) = global_scopes(&projects_dir).unwrap();
        assert!(unread.is_empty(), "{unread:?}");
        let sweep = restart_all(&scopes, Select::All, false, server.name())
            .unwrap()
            .sweep();

        let lines = lines(&sweep);
        assert_eq!(
            sweep.summary(),
            "Restarted 2, skipped 2, failed 0",
            "{lines:#?}"
        );
        for (session, agent) in [(&signup, "implementer"), (&first_main, "qa")] {
            assert!(
                lines.contains(&format!(
                    "{session}: Skipped agent '{agent}': its session is closed"
                )),
                "{lines:#?}"
            );
            assert!(!tmux::has_session(server.name(), session).unwrap());
        }
        for (session, agent) in [(&login, "reviewer"), (&other_main, "implementer")] {
            assert!(
                lines
                    .iter()
                    .any(|l| l.starts_with(&format!("{session}: Restarted agent '{agent}'"))),
                "{lines:#?}"
            );
        }
    }
}
