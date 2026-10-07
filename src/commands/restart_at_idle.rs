//! The restart a sweep defers: a stale agent `pm upgrade` passed over as
//! busy, or the one that ran it, is marked ([`runtime::mark_restart_at_idle`])
//! and restarted once its turn ends. Its waiter, as it marks the agent
//! idle with nothing unread, starts `pm harness hooks restart-at-idle` in a
//! session of its own, so the restart, which kills the agent's pane and
//! the waiter with it, outlives both.
//!
//! That process checks again before it restarts, as the sweep does
//! (`StaleCheck`): an agent no longer stale, one configured for another
//! harness now (a restart would start its conversation over), or one that
//! would not relaunch has its mark dropped, and is left for the user as
//! `pm doctor` reports it. The mark is kept for the next idle while
//! `[upgrade] restart_agents = false`, while the macOS login keychain does
//! not answer and the agent's harness reads it as it starts (the sweep's
//! hold, [`upgrade_restart`](super::upgrade_restart)), and when the restart
//! refuses an agent that turned busy since. Each outcome goes to the
//! agent's Stop hook log.

use std::path::{Path, PathBuf};

use crate::commands::attention::AgentState;
use crate::error::Result;
use crate::keychain::{self, Answer};
use crate::state::paths;
use crate::state::project::GlobalConfig;
use crate::state::runtime;

use super::agent_restart::agent_restart_many;
use super::agent_restart_all::{Scope, StaleCheck, Verdict};

/// The agent this process runs for — its project root, scope and name —
/// when it is marked for a restart at idle and the config allows one.
pub fn marked() -> Option<(PathBuf, String, String)> {
    let agent = std::env::var("PM_AGENT_NAME")
        .ok()
        .filter(|a| !a.is_empty())?;
    let (root, scope) = paths::agent_scope().ok()?;
    (runtime::restart_at_idle_marked(&root, &scope, &agent)
        && GlobalConfig::load_or_default().upgrade.restarts_agents())
    .then_some((root, scope, agent))
}

/// What to do with a marked agent now.
#[derive(Debug, PartialEq)]
enum Due {
    Restart,
    /// Not now, for the reason given; the mark stays.
    Keep(String),
    /// Not at all, for the reason given.
    Drop(String),
}

fn due(
    scope: &Scope,
    agent: &str,
    global: &GlobalConfig,
    ask_keychain: impl FnOnce() -> Option<Answer>,
) -> Result<Due> {
    if !runtime::restart_at_idle_marked(&scope.root, &scope.name, agent) {
        return Ok(Due::Drop("not marked".to_string()));
    }
    if !global.upgrade.restarts_agents() {
        return Ok(Due::Keep("[upgrade] restart_agents = false".to_string()));
    }
    let check = StaleCheck::read(scope, global)?;
    match check.verdict(scope, agent, AgentState::Idle, false) {
        Verdict::Stale => {}
        Verdict::UpToDate => return Ok(Due::Drop("up to date".to_string())),
        Verdict::Held(why) | Verdict::Unknown(why) => return Ok(Due::Drop(why)),
        Verdict::Unrun | Verdict::Caller => return Ok(Due::Drop("not running".to_string())),
    }
    if let Some((_, why)) = check
        .unlaunchable(scope, &[agent.to_string()])?
        .into_iter()
        .next()
    {
        return Ok(Due::Drop(format!("would not relaunch ({why})")));
    }
    let harness = check.harness(agent)?;
    if harness.reads_keychain() && ask_keychain() == Some(Answer::Hung) {
        return Ok(Due::Keep(format!(
            "the macOS login keychain did not answer `{}` within {}s, and {harness} reads \
             it as it starts",
            keychain::COMMAND,
            keychain::LIMIT.as_secs()
        )));
    }
    Ok(Due::Restart)
}

/// Restart the marked `agent` of `scope` if it is due (module docs), and
/// log what happened.
pub fn run(project_root: &Path, scope: &str, agent: &str, tmux_server: Option<&str>) {
    run_with(
        project_root,
        scope,
        agent,
        tmux_server,
        &GlobalConfig::load_or_default(),
        keychain::check,
    );
}

fn run_with(
    project_root: &Path,
    scope: &str,
    agent: &str,
    tmux_server: Option<&str>,
    global: &GlobalConfig,
    ask_keychain: impl FnOnce() -> Option<Answer>,
) {
    let log = |line: &str| {
        runtime::log_stop_hook(
            project_root,
            scope,
            agent,
            &format!("restart at idle: {line}"),
        )
    };
    let due = Scope::of(project_root, scope).and_then(|s| due(&s, agent, global, ask_keychain));
    match due {
        Ok(Due::Restart) => {}
        Ok(Due::Keep(why)) => return log(&format!("kept for the next idle: {why}")),
        Ok(Due::Drop(why)) => {
            log(&format!("dropped: {why}"));
            let _ = runtime::clear_restart_at_idle(project_root, scope, agent);
            return;
        }
        Err(e) => {
            log(&format!("dropped: {e}"));
            let _ = runtime::clear_restart_at_idle(project_root, scope, agent);
            return;
        }
    }
    let names = [agent.to_string()];
    let mut restarted = agent_restart_many(project_root, scope, &names, false, false, tmux_server);
    restarted.confirm_launches(project_root, scope, tmux_server);
    match restarted.results.pop() {
        Some(Ok(line)) => log(&line),
        Some(Err(e)) => log(&format!("kept for the next idle: {e}")),
        None => {}
    }
    restarted.finish(tmux_server);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::project::UpgradeConfig;
    use crate::testing::TestServer;
    use crate::tmux;
    use tempfile::tempdir;

    fn log_of(root: &Path) -> String {
        std::fs::read_to_string(root.join(".pm/runtime/login/reviewer/stop-hook.log"))
            .unwrap_or_default()
    }

    #[test]
    fn a_marked_stale_idle_agent_restarts_once_config_and_keychain_allow_and_an_up_to_date_one_is_unmarked()
     {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&root, &session, "login", "reviewer");
        let waiting = || {
            tmux::pane_processes(server.name(), &format!("{session}:reviewer"))
                .unwrap()
                .iter()
                .any(|p| p.command.contains("sleep 999"))
        };
        let marked = || runtime::restart_at_idle_marked(&root, "login", "reviewer");
        let run = |global: &GlobalConfig, keychain: Answer| {
            run_with(&root, "login", "reviewer", server.name(), global, || {
                Some(keychain)
            })
        };

        runtime::mark_restart_at_idle(&root, "login", "reviewer").unwrap();
        run(&GlobalConfig::default(), Answer::Answered);
        assert!(waiting(), "up to date, so not restarted");
        assert!(!marked());

        runtime::write_launch_stamp(&root, "login", "reviewer", "old").unwrap();
        runtime::mark_restart_at_idle(&root, "login", "reviewer").unwrap();
        let off = GlobalConfig {
            upgrade: UpgradeConfig {
                restart_agents: Some(false),
            },
            ..GlobalConfig::default()
        };
        run(&off, Answer::Answered);
        assert!(waiting(), "the config turns it off");
        assert!(marked(), "until the config turns it back on");

        run(&GlobalConfig::default(), Answer::Hung);
        assert!(
            waiting(),
            "claude-code reads a keychain that does not answer"
        );
        assert!(marked());

        run(&GlobalConfig::default(), Answer::Answered);
        assert!(!waiting(), "{}", log_of(&root));
        assert!(
            log_of(&root).contains("restart at idle: Restarted agent 'reviewer'"),
            "{}",
            log_of(&root)
        );
        assert!(!marked());
    }

    #[test]
    fn a_marked_agent_that_turned_busy_is_kept_for_its_next_idle() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&root, &session, "login", "reviewer");
        runtime::write_launch_stamp(&root, "login", "reviewer", "old").unwrap();
        runtime::mark_restart_at_idle(&root, "login", "reviewer").unwrap();

        run_with(
            &root,
            "login",
            "reviewer",
            server.name(),
            &GlobalConfig::default(),
            || Some(Answer::Answered),
        );

        assert!(runtime::restart_at_idle_marked(&root, "login", "reviewer"));
        assert!(
            log_of(&root).contains("kept for the next idle: agent 'reviewer' is mid-turn"),
            "{}",
            log_of(&root)
        );
    }
}
