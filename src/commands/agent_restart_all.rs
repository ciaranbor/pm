//! `pm agent restart --all`: restart every active agent of a scope, or of
//! every scope of every registered project, through the same per-scope
//! restart as `pm agent restart <names>`
//! ([`agent_restart_many`](super::agent_restart::agent_restart_many)).
//!
//! Which agents to restart is decided from one scan of the server
//! ([`plan()`]); each scope's restart reads its agents again, so
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
//! Each harness's agents restart behind a canary ([`canary`]).
//!
//! [`Select::Stale`] (`--stale`, and `pm update`) narrows the sweep to
//! running agents whose launch is stale ([`stale`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::harness::Harness;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig, ProjectEntry};
use crate::state::runtime;
use crate::state::scope;
use crate::tmux;

use super::agent_restart::{Restarted, callers_agent};
use super::launch_check::{self, FailedLaunch, Launch};

pub mod canary;
pub mod plan;
pub mod stale;
use super::agent_spawn;
use canary::Confirm;
pub use plan::{Plan, Select, plan};
use plan::{held_back, restart_command};
pub(super) use stale::{StaleCheck, Verdict};

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
    let main = Scope::of(root, scope::MAIN)?;
    Ok(scope::names(root)?
        .into_iter()
        .map(|name| Scope {
            name,
            ..main.clone()
        })
        .collect())
}

/// Every scope of every project registered in `projects_dir` that is on this
/// machine, and a skipped report for each project that can't be read.
pub fn global_scopes(projects_dir: &Path) -> Result<(Vec<Scope>, Vec<Report>)> {
    let registry = ProjectEntry::scan(projects_dir)?;
    let mut scopes = Vec::new();
    let mut unread = Vec::new();
    for (name, entry) in registry.projects {
        if !entry.presence().is_here() {
            continue;
        }
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
    /// Left to restart at its next idle.
    Deferred(String),
    /// Restarted, but its harness did not come up.
    NotUp(String),
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
        let (Outcome::Restarted(line)
        | Outcome::Deferred(line)
        | Outcome::NotUp(line)
        | Outcome::Skipped(line)
        | Outcome::Failed(line)) = &self.outcome;
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

    /// `Restarted N, skipped N, failed N`, how many did not come up, and
    /// how many were interrupted.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "Restarted {}",
            self.count(|o| matches!(o, Outcome::Restarted(_)))
        );
        let not_up = self.count(|o| matches!(o, Outcome::NotUp(_)));
        if not_up > 0 {
            line.push_str(&format!(", not up {not_up}"));
        }
        let deferred = self.count(|o| matches!(o, Outcome::Deferred(_)));
        if deferred > 0 {
            line.push_str(&format!(", deferred {deferred}"));
        }
        line.push_str(&format!(
            ", skipped {}, failed {}",
            self.count(|o| matches!(o, Outcome::Skipped(_))),
            self.count(|o| matches!(o, Outcome::Failed(_))),
        ));
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
    /// Whether its launches were already confirmed: a canary's.
    confirmed: bool,
}

/// The launches of `batches` not yet confirmed.
fn launches(batches: &[Batch]) -> Vec<Launch> {
    batches
        .iter()
        .filter(|b| !b.confirmed)
        .flat_map(|b| {
            b.done.launched().map(|agent| Launch {
                project_root: b.scope.root.clone(),
                scope: b.scope.name.clone(),
                agent: agent.to_string(),
            })
        })
        .collect()
}

/// Record each of `failed` in its batch, marking each batch's launches
/// confirmed.
fn record(batches: &mut [Batch], mut failed: Vec<FailedLaunch>) {
    for batch in batches.iter_mut().filter(|b| !b.confirmed) {
        let (mine, rest) = failed.into_iter().partition(|f| {
            f.launch.project_root == batch.scope.root && f.launch.scope == batch.scope.name
        });
        failed = rest;
        batch.done.record_failures(mine);
        batch.confirmed = true;
    }
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
    /// launch or did not come up into a failure saying why. Every launch
    /// not yet confirmed is watched at once.
    pub fn confirm_launches(&mut self, tmux_server: Option<&str>) {
        let failed = launch_check::confirm_all(&launches(&self.batches), tmux_server);
        record(&mut self.batches, failed);
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
                        if batch.done.not_up.contains(agent) {
                            Outcome::NotUp(format!("Restarted agent '{agent}'; {why}"))
                        } else {
                            Outcome::Failed(format!("Failed to restart agent '{agent}': {why}"))
                        }
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

/// The harness each of `names` but the caller launches on its restart;
/// one that cannot be told is left out, so it is no canary and none holds
/// it back.
pub(super) fn harnesses_of(
    scope: &Scope,
    names: &[String],
    global: &GlobalConfig,
    tmux_server: Option<&str>,
) -> HashMap<String, Harness> {
    let (Ok(config), Ok(registry)) = (
        ProjectConfig::load(&paths::pm_dir(&scope.root)),
        AgentRegistry::load(&paths::agents_dir(&scope.root), &scope.name),
    ) else {
        return HashMap::new();
    };
    let caller = callers_agent(&scope.root, &scope.name, names, tmux_server);
    names
        .iter()
        .filter(|name| Some(*name) != caller)
        .filter_map(|name| {
            let entry = registry.get(name)?;
            let harness = agent_spawn::configured_harness(
                entry.effective_definition(name),
                &config.agents,
                &global.agents,
            )
            .ok()?;
            Some((name.clone(), harness))
        })
        .collect()
}

/// Restart the agents of `scopes` that `select` picks (module docs).
pub fn restart_all(
    scopes: &[Scope],
    select: Select,
    force: bool,
    tmux_server: Option<&str>,
) -> Result<RestartAll> {
    let plan = plan(scopes, select, force, tmux_server)?;
    Ok(restart_plan(plan, select, force, tmux_server))
}

/// Restart what `plan`, made by [`plan()`] with `select` and `force`, holds.
pub fn restart_plan(
    plan: Plan,
    select: Select,
    force: bool,
    tmux_server: Option<&str>,
) -> RestartAll {
    let confirm = |launches: &[Launch]| launch_check::confirm_all(launches, tmux_server);
    restart_confirming(plan, select, force, tmux_server, &confirm)
}

/// [`restart_plan`], its canaries' launches confirmed by `confirm`.
fn restart_confirming(
    mut plan: Plan,
    select: Select,
    force: bool,
    tmux_server: Option<&str>,
    confirm: Confirm,
) -> RestartAll {
    for (scope, agent, why) in std::mem::take(&mut plan.deferred) {
        let outcome = match runtime::mark_restart_at_idle(&scope.root, &scope.name, &agent) {
            Ok(()) => Outcome::Deferred(format!(
                "Deferred agent '{agent}': {why}; it restarts at its next idle"
            )),
            Err(e) => Outcome::Failed(format!(
                "Failed to defer agent '{agent}' to its next idle ({e}); restart it once idle \
                 with {}",
                restart_command(&scope, &agent)
            )),
        };
        plan.unrestarted.push(Report {
            target: scope.label(),
            outcome,
        });
    }
    let (batches, held) = canary::restart_planned(plan.planned, force, tmux_server, confirm);
    plan.unrestarted.extend(held);
    RestartAll {
        unrestarted: plan.unrestarted,
        batches,
        select,
        up_to_date: plan.up_to_date,
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
        let (project, _, project_name) = server.setup_project_with_feature(dir.path(), "login");
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
    fn stale_restarts_only_running_stale_agents_and_defers_a_busy_one_to_its_next_idle() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _, project_name) = server.setup_project_with_feature(dir.path(), "login");
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
                "{session}: Deferred agent 'implementer': it is mid-turn; it restarts at its \
                 next idle"
            )
        );
        assert!(
            lines[1].starts_with(&format!("{session}: Restarted agent 'reviewer'")),
            "{lines:#?}"
        );
        assert_eq!(
            sweep.summary(),
            "Restarted 1, deferred 1, skipped 0, failed 0, 1 up to date"
        );
        for (agent, marked) in [("implementer", true), ("reviewer", false), ("qa", false)] {
            assert_eq!(
                crate::state::runtime::restart_at_idle_marked(&project, "login", agent),
                marked,
                "{agent}"
            );
        }
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
        let (project, _, project_name) = server.setup_project_with_feature(dir.path(), "login");
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
                 `pm agent restart reviewer --scope login --project {project_name}` does that"
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
        let (project, _, project_name) = server.setup_project_with_feature(dir.path(), "login");
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
        let (first, projects_dir, first_name) =
            server.setup_project_with_feature(dir.path(), "login");
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

    #[test]
    fn an_agent_whose_harness_does_not_come_up_holds_back_the_rest_of_its_harness() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        for agent in ["implementer", "reviewer"] {
            server.spawn_idle_fake_agent(&project, &session, "login", agent);
            let runtime = crate::state::runtime::agent_dir(&project, "login", agent).unwrap();
            std::fs::write(runtime.join(crate::testing::HOLD_START), "").unwrap();
            // The session before the restart had started: a restart must not
            // read the new one as up on its stamp.
            crate::state::runtime::mark_started(&project, "login", agent).unwrap();
        }
        let confirm = |launches: &[Launch]| {
            launch_check::confirm_within(
                launches,
                server.name(),
                launch_check::START_WITHIN,
                std::time::Duration::from_secs(1),
            )
        };

        let scope = Scope::of(&project, "login").unwrap();
        let plan = plan(&[scope], Select::All, false, server.name()).unwrap();
        let mut done = restart_confirming(plan, Select::All, false, server.name(), &confirm);
        done.confirm_launches(server.name());
        let sweep = done.sweep();

        let lines = lines(&sweep);
        assert_eq!(
            sweep.summary(),
            "Restarted 0, not up 1, skipped 1, failed 0",
            "{lines:#?}"
        );
        assert_eq!(
            lines[0],
            format!(
                "{session}: Skipped agent 'reviewer': not restarted, since {session}'s agent \
                 'implementer', also on claude-code, did not come up after its restart (it has \
                 drawn nothing); restart it once that clears with \
                 `pm agent restart reviewer --scope login --project {project_name}`"
            )
        );
        assert!(
            lines[1].starts_with(&format!(
                "{session}: Restarted agent 'implementer'; its claude-code harness started but \
                 has not come up after 1s"
            )),
            "{lines:#?}"
        );
        let runs = |agent: &str, what: &str| {
            tmux::pane_processes(server.name(), &format!("{session}:{agent}"))
                .unwrap()
                .iter()
                .any(|p| p.command.contains(what))
        };
        assert!(
            runs("reviewer", crate::commands::hooks_install::PM_HOOK_MARKER),
            "the one held back keeps its old session"
        );
        assert!(runs("implementer", "claude"), "the canary is left running");
    }
}
