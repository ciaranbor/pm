//! `pm upgrade`'s last step: restart the agents it left stale
//! ([`launch_stamp`](super::launch_stamp)), through `pm agent restart --all
//! --stale`'s sweep ([`Select::Stale`]), after the new assets and hooks are
//! installed so the stamps compare against them. Idle and unarmed agents
//! restart; busy ones, and the agent running the upgrade, are reported and
//! left stale until a later upgrade or a restart by hand. Dead agents and
//! those of a closed session are left alone: their next spawn launches
//! with what is current. `[upgrade] restart_agents = false` in the global
//! config turns it off.
//!
//! Before it restarts an agent whose harness reads the macOS login keychain
//! as it starts ([`Harness::reads_keychain`]), it asks the keychain
//! ([`keychain`]): one that does not answer would hold each such agent
//! before its session starts, so those are left running and the rest
//! restart. One that answers with an error, as a locked keychain does over
//! ssh, is noted and the sweep goes ahead: a headless Mac's keychain is
//! routinely locked, and the sweep's canaries stop it if the harnesses do
//! not come up.

use std::collections::HashMap;
use std::path::Path;

use crate::harness::Harness;
use crate::keychain::{self, Answer};
use crate::state::project::GlobalConfig;

use super::agent_restart_all::{
    Scope, Select, global_scopes, harnesses_of, plan, project_scopes, restart_plan,
};

/// The scopes `pm upgrade` sweeps: the project at `project_root`'s, else
/// every registered project's; and a line for each project it can't read.
pub fn scopes(projects_dir: &Path, project_root: Option<&Path>) -> (Vec<Scope>, Vec<String>) {
    let found = match project_root {
        Some(root) => project_scopes(root).map(|scopes| (scopes, Vec::new())),
        None => global_scopes(projects_dir)
            .map(|(scopes, unread)| (scopes, unread.iter().map(ToString::to_string).collect())),
    };
    found.unwrap_or_else(|e| (Vec::new(), vec![format!("stale agents: error: {e}")]))
}

/// Restart the stale agents of `scopes`, or with `dry_run` say which it
/// would: a line per agent restarted, skipped or failed, then a summary.
/// Nothing when no agent is stale, or the config turns it off.
pub fn restart_stale(
    scopes: &[Scope],
    global: &GlobalConfig,
    dry_run: bool,
    tmux_server: Option<&str>,
) -> Vec<String> {
    restart_stale_asking(scopes, global, dry_run, tmux_server, keychain::check)
}

/// [`restart_stale`], asking the keychain with `ask_keychain`.
fn restart_stale_asking(
    scopes: &[Scope],
    global: &GlobalConfig,
    dry_run: bool,
    tmux_server: Option<&str>,
    ask_keychain: impl FnOnce() -> Option<Answer>,
) -> Vec<String> {
    if global.upgrade.restart_agents == Some(false) {
        return Vec::new();
    }
    let mut plan = match plan(scopes, Select::Stale, false, tmux_server) {
        Ok(plan) => plan,
        Err(e) => return vec![format!("stale agents: error: {e}")],
    };
    // What each restart launches, as the canaries resolve it.
    let harnesses: Vec<(Scope, HashMap<String, Harness>)> = plan
        .planned
        .iter()
        .map(|(scope, names, _)| {
            let found = harnesses_of(scope, names, global, tmux_server);
            (scope.clone(), found)
        })
        .collect();
    let reads_keychain = |scope: &Scope, agent: &str| {
        harnesses
            .iter()
            .find(|(s, _)| s.root == scope.root && s.name == scope.name)?
            .1
            .get(agent)
            .copied()
            .filter(|harness| harness.reads_keychain())
    };
    let any_reads = harnesses
        .iter()
        .any(|(_, found)| found.values().any(|h| h.reads_keychain()));
    let mut lines = Vec::new();
    match any_reads.then(ask_keychain).flatten() {
        Some(Answer::Hung) => plan.hold(|scope, agent| {
            reads_keychain(scope, agent).map(|harness: Harness| {
                format!(
                    "it is stale, but the macOS login keychain did not answer `{}` within \
                     {}s, and {harness} reads it as it starts",
                    keychain::COMMAND,
                    keychain::LIMIT.as_secs()
                )
            })
        }),
        Some(Answer::Error(said)) => lines.push(format!(
            "Stale agents: the macOS login keychain answered `{}` with an error ({said}); \
             restarting anyway, each harness's agents after one of them comes up",
            keychain::COMMAND
        )),
        _ => {}
    }
    if dry_run {
        lines.extend(plan.would_restart());
        lines.extend(plan.unrestarted.iter().map(ToString::to_string));
        return lines;
    }
    let mut done = restart_plan(plan, Select::Stale, false, tmux_server);
    done.confirm_launches(tmux_server);
    let sweep = done.sweep();
    done.finish(tmux_server);
    if sweep.reports.is_empty() {
        return lines;
    }
    lines.extend(sweep.reports.iter().map(ToString::to_string));
    lines.push(format!("Stale agents: {}", sweep.summary()));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::project::UpgradeConfig;
    use crate::testing::TestServer;
    use crate::tmux;
    use tempfile::tempdir;

    #[test]
    fn a_stale_idle_agent_is_restarted_unless_config_turns_it_off_and_a_dry_run_only_says_so() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project, &session, "login", "reviewer");
        crate::state::runtime::write_launch_stamp(&project, "login", "reviewer", "old").unwrap();
        let (scopes, unread) = scopes(dir.path(), Some(&project));
        assert!(unread.is_empty(), "{unread:?}");
        let waiting = || {
            tmux::pane_processes(server.name(), &format!("{session}:reviewer"))
                .unwrap()
                .iter()
                .any(|p| p.command.contains("sleep 999"))
        };
        let off = GlobalConfig {
            upgrade: UpgradeConfig {
                restart_agents: Some(false),
            },
            ..GlobalConfig::default()
        };

        let answered = || Some(Answer::Answered);
        let restart = |global: &GlobalConfig, dry_run: bool| {
            restart_stale_asking(&scopes, global, dry_run, server.name(), answered)
        };
        assert!(restart(&off, false).is_empty());
        assert_eq!(
            restart(&GlobalConfig::default(), true),
            [format!("{session}: Would restart agent 'reviewer'")]
        );
        assert!(waiting(), "neither restarts it");

        let lines = restart(&GlobalConfig::default(), false);
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(
            lines[0].starts_with(&format!("{session}: Restarted agent 'reviewer'")),
            "{lines:#?}"
        );
        assert_eq!(lines[1], "Stale agents: Restarted 1, skipped 0, failed 0");
        assert!(!waiting());
    }

    #[test]
    fn the_keychain_holds_back_its_readers_while_it_hangs_and_an_error_is_only_noted() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project, &session, "login", "reviewer");
        crate::state::runtime::write_launch_stamp(&project, "login", "reviewer", "old").unwrap();
        let (scopes, _) = scopes(dir.path(), Some(&project));
        let global = GlobalConfig::default();
        let waiting = || {
            tmux::pane_processes(server.name(), &format!("{session}:reviewer"))
                .unwrap()
                .iter()
                .any(|p| p.command.contains("sleep 999"))
        };

        let hung = restart_stale_asking(&scopes, &global, false, server.name(), || {
            Some(Answer::Hung)
        });
        assert_eq!(
            hung,
            [
                format!(
                    "{session}: Skipped agent 'reviewer': it is stale, but the macOS login \
                     keychain did not answer `{}` within 5s, and claude-code reads it as it \
                     starts; restart it once that clears with \
                     `pm agent restart reviewer --scope login`",
                    keychain::COMMAND
                ),
                "Stale agents: Restarted 0, skipped 1, failed 0".to_string(),
            ]
        );
        assert!(waiting(), "it keeps running");

        let locked = restart_stale_asking(&scopes, &global, false, server.name(), || {
            Some(Answer::Error("User interaction is not allowed.".into()))
        });
        assert_eq!(locked.len(), 3, "{locked:#?}");
        assert!(
            locked[0].contains("with an error (User interaction is not allowed.); restarting"),
            "{locked:#?}"
        );
        assert!(
            locked[1].starts_with(&format!("{session}: Restarted agent 'reviewer'")),
            "{locked:#?}"
        );
        assert!(!waiting());

        let asked = std::cell::Cell::new(false);
        restart_stale_asking(&scopes, &global, false, server.name(), || {
            asked.set(true);
            None
        });
        assert!(!asked.get(), "nothing stale, so the keychain is not asked");
    }
}
