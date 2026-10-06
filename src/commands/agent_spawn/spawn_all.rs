//! Respawning every active agent of a scope, best-effort.

use std::path::Path;

use crate::commands::launch_check::{self, FailedLaunch};
use crate::error::Result;
use crate::state::agent::{AgentRegistry, AgentType};
use crate::state::paths;

use super::agent_spawn;

/// Result of `agent_spawn_all`, providing structured counts alongside messages.
pub struct SpawnAllResult {
    /// Human-readable success messages (one per registered agent processed
    /// without error — includes both newly-spawned and already-active agents).
    pub successes: Vec<String>,
    /// Human-readable error messages (one per failed agent).
    pub errors: Vec<String>,
    /// Number of agents that actually had a new tmux window created
    /// ([`SpawnOutcome::Spawned`](super::SpawnOutcome::Spawned) or
    /// [`SpawnOutcome::Resumed`](super::SpawnOutcome::Resumed)). Excludes
    /// already-active no-ops, so idempotent re-runs report `0`.
    pub spawned_count: usize,
    /// Each agent that had a new window created, by its index in
    /// `successes`.
    launched: Vec<(usize, String)>,
}

impl SpawnAllResult {
    /// Move each launched agent whose harness exited at launch from
    /// `successes` to `errors`, saying why ([`launch_check`]).
    pub fn confirm_launches(
        &mut self,
        project_root: &Path,
        scope: &str,
        tmux_server: Option<&str>,
    ) {
        let names: Vec<String> = self.launched.iter().map(|(_, n)| n.clone()).collect();
        let failed = launch_check::confirm(project_root, scope, &names, tmux_server);
        self.record_failures(failed);
    }

    /// The agents that had a new window created.
    pub fn launched(&self) -> impl Iterator<Item = &str> {
        self.launched.iter().map(|(_, name)| name.as_str())
    }

    fn record_failures(&mut self, failed: Vec<FailedLaunch>) {
        let mut exited: Vec<usize> = self
            .launched
            .iter()
            .filter(|(_, name)| failed.iter().any(|f| f.launch.agent == *name))
            .map(|(at, _)| *at)
            .collect();
        exited.sort_unstable_by(|a, b| b.cmp(a));
        for at in exited {
            self.successes.remove(at);
        }
        self.launched.clear();
        self.errors
            .extend(failed.iter().map(|failure| failure.message()));
    }
}

/// Respawn all registered agents for a feature (excludes user-type entries).
/// Best-effort: tries every agent and collects errors rather than failing
/// on the first bad entry.
pub fn agent_spawn_all(
    project_root: &Path,
    feature: &str,
    tmux_server: Option<&str>,
) -> Result<SpawnAllResult> {
    let agents_dir = paths::agents_dir(project_root);
    let registry = AgentRegistry::load(&agents_dir, feature)?;

    let agent_names: Vec<String> = registry
        .agents
        .iter()
        .filter(|(_, e)| e.agent_type == AgentType::Agent && e.active)
        .map(|(n, _)| n.clone())
        .collect();

    if agent_names.is_empty() {
        return Ok(SpawnAllResult {
            successes: vec!["No agents to respawn".to_string()],
            errors: vec![],
            spawned_count: 0,
            launched: Vec::new(),
        });
    }

    let mut successes = Vec::new();
    let mut errors = Vec::new();
    let mut spawned_count = 0;
    let mut launched = Vec::new();
    // `agent_spawn` reads the stored `agent_definition` from the registry
    // when called with `None`, so respawns automatically preserve aliases.
    for name in &agent_names {
        match agent_spawn(project_root, feature, name, None, None, tmux_server) {
            Ok((outcome, msg, _)) => {
                if outcome.is_new_window() {
                    spawned_count += 1;
                    launched.push((successes.len(), name.clone()));
                }
                successes.push(msg);
            }
            Err(e) => errors.push(format!("Failed to spawn '{name}': {e}")),
        }
    }

    Ok(SpawnAllResult {
        successes,
        errors,
        spawned_count,
        launched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_spawn::*;

    use crate::commands::agent_spawn::test_support::*;

    use crate::harness::Harness;
    use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};

    use crate::commands::launch_check::Launch;

    use crate::testing::TestServer;

    use tempfile::tempdir;

    #[test]
    fn spawn_all_respawns_agents() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn two agents
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        agent_spawn(dir.path(), &feature, "tester", None, None, server.name()).unwrap();

        // Kill the session and recreate it (simulating restart — windows gone)
        tmux::kill_session(server.name(), &session_name).unwrap();
        let worktree = dir.path().join("login");
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        // Respawn all
        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(result.spawned_count, 2);
        assert_eq!(result.successes.len(), 2);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn spawn_all_reports_an_agent_that_exited_at_launch_as_an_error_not_a_success() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        agent_spawn(dir.path(), &feature, "tester", None, None, server.name()).unwrap();
        tmux::kill_window(server.name(), &format!("{session_name}:tester")).unwrap();

        let mut result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(result.spawned_count, 1);
        result.record_failures(vec![FailedLaunch {
            launch: Launch {
                project_root: dir.path().to_path_buf(),
                scope: feature.clone(),
                agent: "tester".into(),
            },
            output: "  error: bad flag".into(),
        }]);

        assert_eq!(result.successes.len(), 1, "{:?}", result.successes);
        assert!(result.successes[0].contains("'reviewer' already active"));
        assert_eq!(result.errors.len(), 1);
        assert!(
            result.errors[0].starts_with("agent 'tester': its harness exited at launch")
                && result.errors[0].ends_with("error: bad flag"),
            "{}",
            result.errors[0]
        );
    }

    #[test]
    fn spawn_all_skips_inactive_agents() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        // Spawn two agents
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        agent_spawn(dir.path(), &feature, "tester", None, None, server.name()).unwrap();

        // Mark tester as inactive (simulating `pm agent stop tester`)
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.get_mut("tester").unwrap().active = false;
        registry.save(&agents_dir, &feature).unwrap();

        // Kill the session and recreate it (simulating restart — windows gone)
        tmux::kill_session(server.name(), &session_name).unwrap();
        let worktree = dir.path().join("login");
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        // Respawn all — should only spawn reviewer
        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(result.spawned_count, 1);
        assert_eq!(result.successes.len(), 1);
        assert!(result.successes[0].contains("reviewer"));
        assert!(result.errors.is_empty());

        // reviewer window should exist, tester should not
        assert!(
            tmux::find_window(server.name(), &session_name, "reviewer")
                .unwrap()
                .is_some()
        );
        assert!(
            tmux::find_window(server.name(), &session_name, "tester")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn spawn_all_idempotent_reports_zero_spawned() {
        // Regression for "pm open says Respawned N agents on every run".
        // When agents are already active, spawn_all should report
        // spawned_count == 0 even though every call succeeded.
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();
        agent_spawn(dir.path(), &feature, "tester", None, None, server.name()).unwrap();

        // Call spawn_all without killing windows: every agent is already active.
        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();

        assert_eq!(
            result.spawned_count, 0,
            "no new windows should have been created; got spawned_count={}",
            result.spawned_count
        );
        // We still get success messages for every agent (they just say "already active").
        assert_eq!(result.successes.len(), 2);
        assert!(
            result
                .successes
                .iter()
                .all(|s| s.contains("already active"))
        );
        assert!(result.errors.is_empty());
    }

    #[test]
    fn spawn_all_no_agents_returns_message() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(result.spawned_count, 0);
        assert_eq!(result.successes, vec!["No agents to respawn"]);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn spawn_all_partial_failure_continues() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (_session_name, feature) = setup_project(dir.path(), &server);

        // Spawn a good agent first
        agent_spawn(dir.path(), &feature, "reviewer", None, None, server.name()).unwrap();

        // Manually register a second agent, then destroy the tmux session
        // so that spawning new windows fails for both.
        let agents_dir = paths::agents_dir(dir.path());
        let mut registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        registry.register(
            "tester",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "tester".to_string(),
                active: true,
                agent_definition: None,
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, &feature).unwrap();

        // Kill the session entirely — now both spawns will fail because
        // there's no tmux session to create windows in.
        let pm_dir = paths::pm_dir(dir.path());
        let config = ProjectConfig::load(&pm_dir).unwrap();
        let session_name = tmux::session_name(&config.project.name, &feature);
        tmux::kill_session(server.name(), &session_name).unwrap();

        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();

        // Both should fail, but we get errors for both — not just the first
        assert_eq!(result.spawned_count, 0);
        assert!(result.successes.is_empty());
        assert_eq!(result.errors.len(), 2);
        assert!(result.errors[0].contains("Failed to spawn"));
        assert!(result.errors[1].contains("Failed to spawn"));
    }

    #[test]
    fn spawn_all_preserves_alias_on_respawn() {
        // After spawning an aliased agent, killing its window, and
        // calling `agent_spawn_all`, the respawn should still launch
        // `claude --agent <definition>` (preserved via the registry).
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (session_name, feature) = setup_project(dir.path(), &server);

        agent_spawn(
            dir.path(),
            &feature,
            "frontend-dev",
            Some("implementer"),
            None,
            server.name(),
        )
        .unwrap();

        // Kill and recreate the session (simulating restart — windows gone)
        tmux::kill_session(server.name(), &session_name).unwrap();
        let worktree = dir.path().join("login");
        tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        let result = agent_spawn_all(dir.path(), &feature, server.name()).unwrap();
        assert_eq!(result.spawned_count, 1);

        // Window restored under the display name
        assert!(
            tmux::find_window(server.name(), &session_name, "frontend-dev")
                .unwrap()
                .is_some()
        );

        // Registry still records the definition
        let agents_dir = paths::agents_dir(dir.path());
        let registry = AgentRegistry::load(&agents_dir, &feature).unwrap();
        let entry = registry.get("frontend-dev").unwrap();
        assert_eq!(entry.agent_definition.as_deref(), Some("implementer"));
    }
}
