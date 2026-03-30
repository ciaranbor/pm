use std::path::Path;

use crate::error::{PmError, Result};
use crate::hooks;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Open a project: ensure all tmux sessions exist.
///
/// Creates the `<project>/main` session if missing, then creates sessions for
/// any active features that are missing their sessions. Existing sessions are
/// left untouched (resurrect-aware).
///
/// Features in `initializing` state are skipped — those represent incomplete
/// creations that `pm doctor` should handle.
///
/// Worktree directories that are missing on disk are skipped with a warning
/// printed to stderr rather than aborting the entire open.
///
/// The `tmux_server` parameter allows tests to use an isolated tmux server.
pub fn open(project_root: &Path, tmux_server: Option<&str>) -> Result<()> {
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Backfill hook scripts for projects created before lifecycle hooks existed
    hooks::bootstrap(project_root)?;

    // Ensure <project>/main session exists
    let main_session = format!("{project_name}/main");
    let restore_hook = project_root.join(hooks::RESTORE_PATH);
    if !tmux::has_session(tmux_server, &main_session)? {
        let main_path = project_root.join("main");
        if !main_path.exists() {
            return Err(PmError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("main worktree missing: {}", main_path.display()),
            )));
        }
        tmux::create_session(tmux_server, &main_session, &main_path)?;
        hooks::run_hook(tmux_server, &main_session, &main_path, &restore_hook);
    }

    // Ensure sessions exist for all active features
    let features_dir = paths::features_dir(project_root);
    let features = FeatureState::list(&features_dir)?;

    for (name, state) in &features {
        if !state.status.is_active() {
            continue;
        }
        let session_name = format!("{project_name}/{name}");
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
        }
    }

    Ok(())
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
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        // Kill the main session that init created
        tmux::kill_session(server.name(), &format!("{name}/main")).unwrap();
        assert!(!tmux::has_session(server.name(), &format!("{name}/main")).unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &format!("{name}/main")).unwrap());
    }

    #[test]
    fn open_skips_existing_main_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        // Main session already exists from init — open should not fail
        assert!(tmux::has_session(server.name(), &format!("{name}/main")).unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &format!("{name}/main")).unwrap());
    }

    #[test]
    fn open_recreates_missing_feature_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        // Kill the feature session
        tmux::kill_session(server.name(), &format!("{name}/login")).unwrap();
        assert!(!tmux::has_session(server.name(), &format!("{name}/login")).unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &format!("{name}/login")).unwrap());
    }

    #[test]
    fn open_skips_existing_feature_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        // Feature session exists — open should not fail
        assert!(tmux::has_session(server.name(), &format!("{name}/login")).unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &format!("{name}/login")).unwrap());
    }

    #[test]
    fn open_skips_merged_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        // Manually set feature status to merged
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.status = crate::state::feature::FeatureStatus::Merged;
        state.save(&features_dir, "login").unwrap();

        // Kill the feature session
        tmux::kill_session(server.name(), &format!("{name}/login")).unwrap();

        open(&project_path, server.name()).unwrap();

        // Should NOT recreate session for merged feature
        assert!(!tmux::has_session(server.name(), &format!("{name}/login")).unwrap());
    }

    #[test]
    fn open_with_no_features_only_creates_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        // Kill main
        tmux::kill_session(server.name(), &format!("{name}/main")).unwrap();

        open(&project_path, server.name()).unwrap();

        let sessions: Vec<_> = tmux::list_sessions(server.name())
            .unwrap()
            .into_iter()
            .filter(|s| s.starts_with(&format!("{name}/")))
            .collect();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0], format!("{name}/main"));
    }

    #[test]
    fn open_backfills_hook_scripts_for_existing_projects() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        // Simulate a pre-hooks project by removing the bootstrapped hooks
        std::fs::remove_dir_all(project_path.join(".pm/hooks")).unwrap();
        assert!(!project_path.join(hooks::POST_CREATE_PATH).exists());

        open(&project_path, server.name()).unwrap();

        assert!(project_path.join(hooks::POST_CREATE_PATH).is_file());
        assert!(project_path.join(hooks::POST_MERGE_PATH).is_file());
    }

    #[test]
    fn open_errors_when_main_worktree_missing() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let name = server.scope("myapp");
        let project_path = dir.path().join(&name);
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        // Kill session and delete the main worktree
        tmux::kill_session(server.name(), &format!("{name}/main")).unwrap();
        std::fs::remove_dir_all(project_path.join("main")).unwrap();

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
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
        )
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
        tmux::kill_session(server.name(), &format!("{name}/main")).unwrap();
        tmux::kill_session(server.name(), &format!("{name}/login")).unwrap();

        open(&project_path, server.name()).unwrap();

        // Verify sessions were created and hook windows exist (restore hook ran)
        assert!(tmux::has_session(server.name(), &format!("{name}/main")).unwrap());
        assert!(tmux::has_session(server.name(), &format!("{name}/login")).unwrap());
        assert!(
            tmux::find_window(server.name(), &format!("{name}/main"), "hook")
                .unwrap()
                .is_some()
        );
        assert!(
            tmux::find_window(server.name(), &format!("{name}/login"), "hook")
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
        init::init(&project_path, &projects_dir, server.name()).unwrap();

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
            tmux::find_window(server.name(), &format!("{name}/main"), "hook")
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
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();
        feat_new::feat_new(&project_path, "api", None, None, None, false, server.name()).unwrap();

        // Kill sessions and delete only login's worktree
        tmux::kill_session(server.name(), &format!("{name}/login")).unwrap();
        tmux::kill_session(server.name(), &format!("{name}/api")).unwrap();
        std::fs::remove_dir_all(project_path.join("login")).unwrap();

        open(&project_path, server.name()).unwrap();

        // login skipped (missing worktree), api recreated
        assert!(!tmux::has_session(server.name(), &format!("{name}/login")).unwrap());
        assert!(tmux::has_session(server.name(), &format!("{name}/api")).unwrap());
    }
}
