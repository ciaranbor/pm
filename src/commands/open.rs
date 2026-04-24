use std::path::Path;

use crate::commands::agent_spawn;
use crate::error::{PmError, Result};
use crate::hooks;
use crate::state::agent::{AgentRegistry, AgentType};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Result of an `open` operation, containing restore statistics.
pub struct OpenResult {
    /// Number of sessions that were created (not already existing).
    pub sessions_restored: usize,
    /// Number of agents that were successfully respawned.
    pub agents_respawned: usize,
}

/// Respawn agents for a given scope.
///
/// Returns the number of agents successfully respawned. If `select_window_zero`
/// is true and no agents were respawned, selects window 0 as the landing window.
fn respawn_agents_for_scope(
    project_root: &Path,
    scope: &str,
    session_name: &str,
    agents_dir: &Path,
    tmux_server: Option<&str>,
    select_window_zero: bool,
) -> Result<usize> {
    let spawn_result = agent_spawn::agent_spawn_all(project_root, scope, tmux_server)?;
    let spawned = spawn_result.spawned_count;
    for err in &spawn_result.errors {
        eprintln!("warning: {err}");
    }

    if spawned > 0 {
        let registry = AgentRegistry::load(agents_dir, scope)?;
        let first_agent = registry
            .agents
            .iter()
            .filter(|(_, e)| e.agent_type == AgentType::Agent)
            .find_map(|(_, e)| {
                tmux::find_window(tmux_server, session_name, &e.window_name)
                    .ok()
                    .flatten()
            });
        if let Some(target) = first_agent {
            let _ = tmux::select_window(tmux_server, &target);
        }
    } else if select_window_zero {
        let _ = tmux::select_window(tmux_server, &format!("{session_name}:0"));
    }

    Ok(spawned)
}

/// Open a project: ensure all tmux sessions exist, then respawn agents.
///
/// Creates the `<project>/main` session if missing, then creates sessions for
/// any active features that are missing their sessions. Existing sessions are
/// left untouched (resurrect-aware).
///
/// After session creation, respawns agents marked `active = true` via
/// `agent_spawn_all`. Agents whose windows already exist are skipped.
///
/// Finally, selects a sensible landing window in each restored session: the
/// first agent window if any agents were respawned, otherwise window 0.
///
/// Features in `initializing` state are skipped — those represent incomplete
/// creations that `pm doctor` should handle.
///
/// Worktree directories that are missing on disk are skipped with a warning
/// printed to stderr rather than aborting the entire open.
///
/// The `tmux_server` parameter allows tests to use an isolated tmux server.
pub fn open(project_root: &Path, tmux_server: Option<&str>) -> Result<OpenResult> {
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Backfill hook scripts for projects created before lifecycle hooks existed
    hooks::bootstrap(project_root)?;

    let mut sessions_restored: usize = 0;
    let mut agents_respawned: usize = 0;
    let agents_dir = paths::agents_dir(project_root);

    // Ensure <project>/main session exists
    let main_session = tmux::session_name(project_name, "main");
    let restore_hook = project_root.join(hooks::RESTORE_PATH);
    if !tmux::has_session(tmux_server, &main_session)? {
        let main_path = paths::main_worktree(project_root);
        if !main_path.exists() {
            return Err(PmError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("main worktree missing: {}", main_path.display()),
            )));
        }
        tmux::create_session(tmux_server, &main_session, &main_path)?;
        hooks::run_hook(tmux_server, &main_session, &main_path, &restore_hook);
        sessions_restored += 1;
    }

    // Respawn agents marked active in the main scope. If the session was just
    // recreated, their windows are gone and agent_spawn will create new ones.
    // If the session already existed, agent_spawn is idempotent (skips agents
    // whose windows are still present).
    agents_respawned += respawn_agents_for_scope(
        project_root,
        "main",
        &main_session,
        &agents_dir,
        tmux_server,
        false,
    )?;

    // Ensure sessions exist for all active features
    let features_dir = paths::features_dir(project_root);
    let features = FeatureState::list(&features_dir)?;

    // Track which features have active sessions (for agent respawn)
    let mut active_features: Vec<String> = Vec::new();

    for (name, state) in &features {
        if !state.status.is_active() {
            continue;
        }
        let session_name = tmux::session_name(project_name, name);
        if !tmux::has_session(tmux_server, &session_name)? {
            let worktree_path = project_root.join(&state.worktree);
            if !worktree_path.exists() {
                eprintln!(
                    "warning: skipping '{name}': worktree missing at {}",
                    worktree_path.display()
                );
                continue;
            }
            tmux::create_session(tmux_server, &session_name, &worktree_path)?;
            hooks::run_hook(tmux_server, &session_name, &worktree_path, &restore_hook);
            sessions_restored += 1;
        }
        active_features.push(name.clone());
    }

    // Respawn agents for ALL active features (not just restored sessions).
    // agent_spawn is idempotent — skips agents whose windows already exist.
    for feature in &active_features {
        let session_name = tmux::session_name(project_name, feature);
        agents_respawned += respawn_agents_for_scope(
            project_root,
            feature,
            &session_name,
            &agents_dir,
            tmux_server,
            true,
        )?;
    }

    Ok(OpenResult {
        sessions_restored,
        agents_respawned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn open_creates_main_session_when_missing() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Kill the main session that init created
        tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();
        assert!(!tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());
    }

    #[test]
    fn open_skips_existing_main_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Main session already exists from init — open should not fail
        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());
    }

    #[test]
    fn open_recreates_missing_feature_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        // Kill the feature session
        tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();
        assert!(!tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
    }

    #[test]
    fn open_skips_existing_feature_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        // Feature session exists — open should not fail
        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
    }

    #[test]
    fn open_skips_merged_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        // Manually set feature status to merged
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.status = crate::state::feature::FeatureStatus::Merged;
        state.save(&features_dir, "login").unwrap();

        // Kill the feature session
        tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();

        open(&project_path, server.name()).unwrap();

        // Should NOT recreate session for merged feature
        assert!(!tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
    }

    #[test]
    fn open_with_no_features_only_creates_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Kill main
        tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();

        open(&project_path, server.name()).unwrap();

        let sessions: Vec<_> = tmux::list_sessions(server.name())
            .unwrap()
            .into_iter()
            .filter(|s| s.starts_with(&format!("{name}/")))
            .collect();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0], tmux::session_name(&name, "main"));
    }

    #[test]
    fn open_backfills_hook_scripts_for_existing_projects() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Simulate a pre-hooks project by removing the bootstrapped hooks
        std::fs::remove_dir_all(project_path.join(".pm/hooks")).unwrap();
        assert!(!project_path.join(hooks::POST_CREATE_PATH).exists());

        open(&project_path, server.name()).unwrap();

        assert!(project_path.join(hooks::POST_CREATE_PATH).is_file());
        assert!(project_path.join(hooks::POST_MERGE_PATH).is_file());
        assert!(project_path.join(hooks::RESTORE_PATH).is_file());
    }

    #[test]
    fn open_errors_when_main_worktree_missing() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Kill session and delete the main worktree
        tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();
        std::fs::remove_dir_all(paths::main_worktree(&project_path)).unwrap();

        let result = open(&project_path, server.name());
        assert!(result.is_err());
    }

    #[test]
    fn open_runs_restore_hook_for_new_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        // Create a restore hook
        let restore_path = project_path.join(hooks::RESTORE_PATH);
        std::fs::write(&restore_path, "#!/bin/sh\necho restored\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&restore_path, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        // Kill all sessions to force recreation
        tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();
        tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();

        open(&project_path, server.name()).unwrap();

        // Verify sessions were created and hook windows exist (restore hook ran)
        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap());
        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
        assert!(
            tmux::find_window(server.name(), &tmux::session_name(&name, "main"), "hook")
                .unwrap()
                .is_some()
        );
        assert!(
            tmux::find_window(server.name(), &tmux::session_name(&name, "login"), "hook")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn open_skips_restore_hook_for_existing_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Create a restore hook
        let restore_path = project_path.join(hooks::RESTORE_PATH);
        std::fs::write(&restore_path, "#!/bin/sh\necho restored\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&restore_path, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        // Sessions already exist from init — open should NOT run restore hook
        open(&project_path, server.name()).unwrap();

        // No hook window should exist since sessions were not recreated
        assert!(
            tmux::find_window(server.name(), &tmux::session_name(&name, "main"), "hook")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn open_skips_feature_with_missing_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "api",
            server.name(),
        ))
        .unwrap();

        // Kill sessions and delete only login's worktree
        tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();
        tmux::kill_session(server.name(), &tmux::session_name(&name, "api")).unwrap();
        std::fs::remove_dir_all(project_path.join("login")).unwrap();

        open(&project_path, server.name()).unwrap();

        // login skipped (missing worktree), api recreated
        assert!(!tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());
        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "api")).unwrap());
    }

    #[test]
    fn open_returns_zero_counts_when_nothing_restored() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // All sessions already exist from init
        let result = open(&project_path, server.name()).unwrap();
        assert_eq!(result.sessions_restored, 0);
        assert_eq!(result.agents_respawned, 0);
    }

    #[test]
    fn open_returns_session_count_when_restored() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        // Kill both sessions
        tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();
        tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();

        let result = open(&project_path, server.name()).unwrap();
        assert_eq!(result.sessions_restored, 2); // main + login
        assert_eq!(result.agents_respawned, 0);
    }

    #[test]
    fn open_respawns_agents_for_restored_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        // Register an agent for the feature
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
                active: true,
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        // Kill the feature session (simulating reboot)
        tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();

        let result = open(&project_path, server.name()).unwrap();

        // Session restored and agent respawned
        assert_eq!(result.sessions_restored, 1);
        assert_eq!(result.agents_respawned, 1);

        // Agent window should exist in the restored session
        assert!(
            tmux::find_window(
                server.name(),
                &tmux::session_name(&name, "login"),
                "reviewer"
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn open_clears_stale_active_flags() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        // Register an agent marked active (but its window won't exist after session kill)
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
                active: true,
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        // Kill the feature session
        tmux::kill_session(server.name(), &tmux::session_name(&name, "login")).unwrap();

        open(&project_path, server.name()).unwrap();

        // After open, the agent should be respawned (window exists)
        let session_name = tmux::session_name(&name, "login");
        let window = tmux::find_window(server.name(), &session_name, "reviewer").unwrap();
        assert!(window.is_some(), "reviewer window should be respawned");
    }

    #[test]
    fn open_respawns_agents_for_main_scope() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Register an agent in the main scope
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register(
            "orchestrator",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "orchestrator".to_string(),
                active: true,
            },
        );
        registry.save(&agents_dir, "main").unwrap();

        // Kill the main session (simulating reboot)
        tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();

        let result = open(&project_path, server.name()).unwrap();

        // Session restored and agent respawned
        assert_eq!(result.sessions_restored, 1);
        assert_eq!(result.agents_respawned, 1);

        // Agent window should exist in the restored main session
        assert!(
            tmux::find_window(
                server.name(),
                &tmux::session_name(&name, "main"),
                "orchestrator"
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn open_respawns_agents_when_session_already_exists() {
        // This is the core bug fix: when tmux-resurrect preserves the session
        // but the agent window is gone, open should still respawn agents.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        // Register an agent marked active (simulating a previously running agent)
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
                active: true,
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        // Session still exists (NOT killed) — simulates tmux-resurrect preserving it.
        // But the agent window doesn't exist (it was in a different window that wasn't preserved).
        assert!(tmux::has_session(server.name(), &tmux::session_name(&name, "login")).unwrap());

        let result = open(&project_path, server.name()).unwrap();

        // Session was NOT restored (it already existed)
        assert_eq!(result.sessions_restored, 0);
        // But agent should still be respawned (exactly one registered)
        assert_eq!(result.agents_respawned, 1);

        // Agent window should exist
        assert!(
            tmux::find_window(
                server.name(),
                &tmux::session_name(&name, "login"),
                "reviewer"
            )
            .unwrap()
            .is_some()
        );
    }
}
