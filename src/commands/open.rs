use std::path::Path;

use crate::error::{PmError, Result};
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

    // Ensure <project>/main session exists
    let main_session = format!("{project_name}/main");
    if !tmux::has_session(tmux_server, &main_session)? {
        let main_path = project_root.join("main");
        if !main_path.exists() {
            return Err(PmError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("main worktree missing: {}", main_path.display()),
            )));
        }
        tmux::create_session(tmux_server, &main_session, &main_path)?;
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
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        // Kill the main session that init created
        tmux::kill_session(server.name(), "myapp/main").unwrap();
        assert!(!tmux::has_session(server.name(), "myapp/main").unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), "myapp/main").unwrap());
    }

    #[test]
    fn open_skips_existing_main_session() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        // Main session already exists from init — open should not fail
        assert!(tmux::has_session(server.name(), "myapp/main").unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), "myapp/main").unwrap());
    }

    #[test]
    fn open_recreates_missing_feature_sessions() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(&project_path, "login", None, server.name()).unwrap();

        // Kill the feature session
        tmux::kill_session(server.name(), "myapp/login").unwrap();
        assert!(!tmux::has_session(server.name(), "myapp/login").unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), "myapp/login").unwrap());
    }

    #[test]
    fn open_skips_existing_feature_sessions() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(&project_path, "login", None, server.name()).unwrap();

        // Feature session exists — open should not fail
        assert!(tmux::has_session(server.name(), "myapp/login").unwrap());

        open(&project_path, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), "myapp/login").unwrap());
    }

    #[test]
    fn open_skips_merged_features() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(&project_path, "login", None, server.name()).unwrap();

        // Manually set feature status to merged
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.status = crate::state::feature::FeatureStatus::Merged;
        state.save(&features_dir, "login").unwrap();

        // Kill the feature session
        tmux::kill_session(server.name(), "myapp/login").unwrap();

        open(&project_path, server.name()).unwrap();

        // Should NOT recreate session for merged feature
        assert!(!tmux::has_session(server.name(), "myapp/login").unwrap());
    }

    #[test]
    fn open_with_no_features_only_creates_main() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        // Kill main
        tmux::kill_session(server.name(), "myapp/main").unwrap();

        open(&project_path, server.name()).unwrap();

        let sessions = tmux::list_sessions(server.name()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0], "myapp/main");
    }

    #[test]
    fn open_errors_when_main_worktree_missing() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        // Kill session and delete the main worktree
        tmux::kill_session(server.name(), "myapp/main").unwrap();
        std::fs::remove_dir_all(project_path.join("main")).unwrap();

        let result = open(&project_path, server.name());
        assert!(result.is_err());
    }

    #[test]
    fn open_skips_feature_with_missing_worktree() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(&project_path, "login", None, server.name()).unwrap();
        feat_new::feat_new(&project_path, "api", None, server.name()).unwrap();

        // Kill sessions and delete only login's worktree
        tmux::kill_session(server.name(), "myapp/login").unwrap();
        tmux::kill_session(server.name(), "myapp/api").unwrap();
        std::fs::remove_dir_all(project_path.join("login")).unwrap();

        open(&project_path, server.name()).unwrap();

        // login skipped (missing worktree), api recreated
        assert!(!tmux::has_session(server.name(), "myapp/login").unwrap());
        assert!(tmux::has_session(server.name(), "myapp/api").unwrap());
    }
}
