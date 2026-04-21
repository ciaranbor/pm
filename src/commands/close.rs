use std::path::Path;

use crate::error::Result;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::tmux;

/// Close a project by killing all its tmux sessions (main + features).
///
/// This is the counterpart to `pm open`. No state or files are deleted.
/// Idempotent — sessions that are already gone are silently skipped.
///
/// Returns the project name and the number of sessions killed.
pub fn close(project_root: &Path, tmux_server: Option<&str>) -> Result<(String, usize)> {
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let features_dir = paths::features_dir(project_root);
    let features = FeatureState::list(&features_dir)?;

    let mut killed = 0;

    // Kill feature sessions first, switching the client to main if needed
    let main_session = format!("{project_name}/main");
    for (name, _) in &features {
        let session_name = format!("{project_name}/{name}");
        if tmux::has_session(tmux_server, &session_name)? {
            let _ = tmux::switch_client(tmux_server, &main_session);
            tmux::kill_session(tmux_server, &session_name)?;
            killed += 1;
        }
    }

    // Kill main session last — switch to an external session first if possible
    if tmux::has_session(tmux_server, &main_session)? {
        let all_sessions = tmux::list_sessions(tmux_server).unwrap_or_default();
        if let Some(external) = all_sessions.iter().find(|s| s.as_str() != main_session) {
            let _ = tmux::switch_client(tmux_server, external);
        }
        tmux::kill_session(tmux_server, &main_session)?;
        killed += 1;
    }

    Ok((project_name.clone(), killed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feat_new;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn close_kills_all_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _projects_dir, project_name) = server.setup_project(dir.path());

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

        // Verify sessions exist
        assert!(tmux::has_session(server.name(), &format!("{project_name}/main")).unwrap());
        assert!(tmux::has_session(server.name(), &format!("{project_name}/login")).unwrap());
        assert!(tmux::has_session(server.name(), &format!("{project_name}/api")).unwrap());

        let (name, killed) = close(&project_path, server.name()).unwrap();

        assert_eq!(name, project_name);
        assert_eq!(killed, 3);
        assert!(!tmux::has_session(server.name(), &format!("{project_name}/main")).unwrap());
        assert!(!tmux::has_session(server.name(), &format!("{project_name}/login")).unwrap());
        assert!(!tmux::has_session(server.name(), &format!("{project_name}/api")).unwrap());
    }

    #[test]
    fn close_preserves_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        close(&project_path, server.name()).unwrap();

        // State files should still exist
        assert!(paths::pm_dir(&project_path).exists());
        assert!(projects_dir.join(format!("{project_name}.toml")).exists());
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        // Worktree directory should still exist
        assert!(project_path.join("login").exists());
    }

    #[test]
    fn close_idempotent_when_sessions_already_gone() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _projects_dir, project_name) = server.setup_project(dir.path());

        // Kill sessions manually first
        let main_session = format!("{project_name}/main");
        tmux::kill_session(server.name(), &main_session).unwrap();

        // close should not error
        let (name, killed) = close(&project_path, server.name()).unwrap();
        assert_eq!(name, project_name);
        assert_eq!(killed, 0);
    }

    #[test]
    fn close_only_main_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _projects_dir, project_name) = server.setup_project(dir.path());

        let (name, killed) = close(&project_path, server.name()).unwrap();
        assert_eq!(name, project_name);
        assert_eq!(killed, 1);
        assert!(!tmux::has_session(server.name(), &format!("{project_name}/main")).unwrap());
    }

    #[test]
    fn close_does_not_kill_other_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _projects_dir, project_name) = server.setup_project(dir.path());

        // Create a session that belongs to a different project
        let other_session = format!("{}-other/main", server.scope(""));
        tmux::create_session(server.name(), &other_session, dir.path()).unwrap();

        close(&project_path, server.name()).unwrap();

        // Our sessions are gone
        assert!(!tmux::has_session(server.name(), &format!("{project_name}/main")).unwrap());
        // The other session is untouched
        assert!(tmux::has_session(server.name(), &other_session).unwrap());

        // Clean up
        tmux::kill_session(server.name(), &other_session).unwrap();
    }
}
