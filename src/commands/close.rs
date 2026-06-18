use std::path::Path;

use crate::error::Result;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};
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

/// Close every project in the global registry by killing their tmux sessions.
///
/// Iterates over all registered projects, running the same non-destructive
/// close as the single-project path. A project with no live sessions is a
/// no-op; per-project failures are reported but never abort the sweep. No
/// state or files are deleted.
///
/// Returns one human-readable status line per project.
pub fn close_all(tmux_server: Option<&str>) -> Result<Vec<String>> {
    let projects_dir = paths::global_projects_dir()?;
    close_all_with_dir(&projects_dir, tmux_server)
}

/// `close_all` with an injectable registry dir (for tests).
pub fn close_all_with_dir(projects_dir: &Path, tmux_server: Option<&str>) -> Result<Vec<String>> {
    let projects = ProjectEntry::list(projects_dir)?;

    if projects.is_empty() {
        return Ok(vec!["No projects in registry".to_string()]);
    }

    let mut messages = Vec::new();
    for (name, entry) in &projects {
        let root = entry.root_path();
        if !root.exists() {
            messages.push(format!("{name}: skipped (directory does not exist)"));
            continue;
        }
        match close(&root, tmux_server) {
            Ok((project_name, killed)) => {
                messages.push(format!(
                    "{project_name}: closed (killed {killed} session{})",
                    if killed == 1 { "" } else { "s" }
                ));
            }
            Err(e) => {
                messages.push(format!("{name}: error: {e}"));
            }
        }
    }

    Ok(messages)
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
    fn close_all_closes_every_project() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let projects_dir = dir.path().join("registry");

        // Two distinct projects sharing one registry
        let name_a = server.scope("alpha");
        let path_a = dir.path().join(&name_a);
        crate::commands::init::init(&path_a, &projects_dir, None, server.name()).unwrap();

        let name_b = server.scope("beta");
        let path_b = dir.path().join(&name_b);
        crate::commands::init::init(&path_b, &projects_dir, None, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), &format!("{name_a}/main")).unwrap());
        assert!(tmux::has_session(server.name(), &format!("{name_b}/main")).unwrap());

        let msgs = close_all_with_dir(&projects_dir, server.name()).unwrap();

        assert!(msgs.iter().any(|m| m.contains(&name_a)), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains(&name_b)), "{msgs:?}");
        assert!(!tmux::has_session(server.name(), &format!("{name_a}/main")).unwrap());
        assert!(!tmux::has_session(server.name(), &format!("{name_b}/main")).unwrap());

        // State is preserved for both
        assert!(paths::pm_dir(&path_a).exists());
        assert!(paths::pm_dir(&path_b).exists());
    }

    #[test]
    fn close_all_no_sessions_is_no_op() {
        // No tmux sessions created — close_all should report killed 0, not error.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (_project_path, projects_dir, project_name) = server.setup_project_no_tmux(dir.path());

        let msgs = close_all_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.contains(&project_name) && m.contains("killed 0 sessions")),
            "{msgs:?}"
        );
    }

    #[test]
    fn close_all_empty_registry() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let projects_dir = dir.path().join("registry");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let msgs = close_all_with_dir(&projects_dir, server.name()).unwrap();
        assert!(msgs.iter().any(|m| m.contains("No projects")), "{msgs:?}");
    }

    #[test]
    fn close_all_skips_missing_directory() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let projects_dir = dir.path().join("registry");

        let entry = ProjectEntry {
            root: dir.path().join("gone").to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "ghost").unwrap();

        let msgs = close_all_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.contains("ghost") && m.contains("directory does not exist")),
            "{msgs:?}"
        );
    }

    #[test]
    fn close_all_reports_per_project_error_and_continues() {
        // A registered dir that exists but has no valid .pm/config.toml makes
        // close() error; close_all should report it and still sweep the rest.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let projects_dir = dir.path().join("registry");

        // Broken project: directory exists, no .pm/ config
        let broken_path = dir.path().join("broken");
        std::fs::create_dir_all(&broken_path).unwrap();
        let broken = ProjectEntry {
            root: broken_path.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        broken.save(&projects_dir, "broken").unwrap();

        // Healthy project alongside it
        let name_ok = server.scope("healthy");
        let path_ok = dir.path().join(&name_ok);
        crate::commands::init::init(&path_ok, &projects_dir, None, server.name()).unwrap();

        let msgs = close_all_with_dir(&projects_dir, server.name()).unwrap();

        assert!(
            msgs.iter()
                .any(|m| m.contains("broken") && m.contains("error")),
            "{msgs:?}"
        );
        // Sweep continued: the healthy project was still closed
        assert!(msgs.iter().any(|m| m.contains(&name_ok)), "{msgs:?}");
        assert!(!tmux::has_session(server.name(), &format!("{name_ok}/main")).unwrap());
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
