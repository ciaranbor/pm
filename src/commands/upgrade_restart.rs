//! `pm upgrade`'s last step: restart the agents it left stale
//! ([`launch_stamp`](super::launch_stamp)), through `pm agent restart --all
//! --stale`'s sweep ([`Select::Stale`]), after the new assets and hooks are
//! installed so the stamps compare against them. Idle and unarmed agents
//! restart; busy ones, and the agent running the upgrade, are reported and
//! left stale until a later upgrade or a restart by hand. Dead agents and
//! those of a closed session are left alone: their next spawn launches
//! with what is current. `[upgrade] restart_agents = false` in the global
//! config turns it off.

use std::path::Path;

use crate::state::project::GlobalConfig;

use super::agent_restart_all::{Scope, Select, global_scopes, plan, project_scopes, restart_all};

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
    if global.upgrade.restart_agents == Some(false) {
        return Vec::new();
    }
    if dry_run {
        return match plan(scopes, Select::Stale, false, tmux_server) {
            Ok(plan) => {
                let mut lines = plan.would_restart();
                lines.extend(plan.unrestarted.iter().map(ToString::to_string));
                lines
            }
            Err(e) => vec![format!("stale agents: error: {e}")],
        };
    }
    let mut done = match restart_all(scopes, Select::Stale, false, tmux_server) {
        Ok(done) => done,
        Err(e) => return vec![format!("stale agents: error: {e}")],
    };
    done.confirm_launches(tmux_server);
    let sweep = done.sweep();
    done.finish(tmux_server);
    if sweep.reports.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = sweep.reports.iter().map(ToString::to_string).collect();
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

        assert!(restart_stale(&scopes, &off, false, server.name()).is_empty());
        assert_eq!(
            restart_stale(&scopes, &GlobalConfig::default(), true, server.name()),
            [format!("{session}: Would restart agent 'reviewer'")]
        );
        assert!(waiting(), "neither restarts it");

        let lines = restart_stale(&scopes, &GlobalConfig::default(), false, server.name());
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(
            lines[0].starts_with(&format!("{session}: Restarted agent 'reviewer'")),
            "{lines:#?}"
        );
        assert_eq!(lines[1], "Stale agents: Restarted 1, skipped 0, failed 0");
        assert!(!waiting());
    }
}
