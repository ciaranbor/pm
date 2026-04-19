use crate::error::Result;
use crate::git;
use crate::state::paths;
use crate::state::project::ProjectEntry;

/// Result of restoring a single project.
struct ProjectResult {
    messages: Vec<String>,
}

/// Restore all projects from the global registry on a fresh machine.
///
/// For each registry entry:
/// 1. If the project dir doesn't exist and `repo_url` is set: `pm init --git <url> <path>`
/// 2. If `state_remote` is set and .pm/ has no remote: set the remote and pull
/// 3. Run `pm open` to recreate tmux sessions
///
/// The `tmux_server` parameter allows tests to use an isolated tmux server.
pub fn restore(tmux_server: Option<&str>) -> Result<Vec<String>> {
    let projects_dir = paths::global_projects_dir()?;
    restore_with_dir(&projects_dir, tmux_server)
}

/// Testable inner function that takes an explicit projects directory.
pub fn restore_with_dir(
    projects_dir: &std::path::Path,
    tmux_server: Option<&str>,
) -> Result<Vec<String>> {
    let projects = ProjectEntry::list(projects_dir)?;

    if projects.is_empty() {
        return Ok(vec!["No projects in registry".to_string()]);
    }

    let mut all_messages = Vec::new();

    for (name, entry) in &projects {
        let result = restore_project(name, entry, projects_dir, tmux_server);
        match result {
            Ok(pr) => {
                all_messages.extend(pr.messages);
            }
            Err(e) => {
                all_messages.push(format!("{name}: error: {e}"));
            }
        }
    }

    Ok(all_messages)
}

fn restore_project(
    name: &str,
    entry: &ProjectEntry,
    projects_dir: &std::path::Path,
    tmux_server: Option<&str>,
) -> Result<ProjectResult> {
    let root = entry.root_path();
    let mut messages = Vec::new();

    // Step 1: Clone if the project directory doesn't exist
    if !root.exists() {
        if let Some(ref repo_url) = entry.repo_url {
            messages.push(format!("{name}: cloning from {repo_url}..."));
            super::init::init(&root, projects_dir, Some(repo_url), tmux_server)?;
            // init creates a fresh registry entry; re-save the original URLs
            // so state_remote isn't lost
            let mut refreshed = match ProjectEntry::load(projects_dir, name) {
                Ok(e) => e,
                Err(crate::error::PmError::ProjectNotFound(_)) => entry.clone(),
                Err(e) => {
                    messages.push(format!(
                        "{name}: warning: failed to reload registry entry: {e}"
                    ));
                    entry.clone()
                }
            };
            refreshed.repo_url = entry.repo_url.clone();
            refreshed.state_remote = entry.state_remote.clone();
            refreshed.save(projects_dir, name)?;
            messages.push(format!("{name}: cloned and initialised"));
        } else {
            messages.push(format!(
                "{name}: skipped (directory does not exist and no repo_url)"
            ));
            return Ok(ProjectResult { messages });
        }
    } else {
        messages.push(format!("{name}: directory exists"));
    }

    // Step 2: Set up .pm/ state remote and pull if needed
    let pm_dir = paths::pm_dir(&root);
    if let Some(ref state_remote_url) = entry.state_remote {
        if pm_dir.join(".git").exists() {
            if !git::has_remote(&pm_dir, "origin")? {
                // Fresh remote: use fetch + reset (not pull) since there's no
                // tracking branch configured yet.
                match super::state_cmd::apply_remote_and_pull(
                    &pm_dir,
                    state_remote_url,
                    "state",
                    true,
                ) {
                    Ok(msg) => {
                        messages.push(format!("{name}: {msg}"));
                    }
                    Err(e) => {
                        messages.push(format!("{name}: .pm/ pull failed: {e}"));
                    }
                }
            } else {
                // Remote already configured — ensure upstream tracking is set
                // before pulling (a previous restore run may have added the
                // remote but failed before pulling, leaving no tracking branch).
                let branch = git::current_branch(&pm_dir)?;
                if git::tracking_branch(&pm_dir, &branch)?.is_none() {
                    let upstream_ref = format!("refs/remotes/origin/{branch}");
                    git::fetch_remote(&pm_dir, "origin")?;
                    if git::ref_exists(&pm_dir, &upstream_ref)? {
                        git::set_upstream(&pm_dir, &upstream_ref)?;
                    }
                }
                // During restore, remote is authoritative. Try pull first;
                // if histories diverge, reset to remote branch.
                match git::pull(&pm_dir) {
                    Ok(()) => {
                        messages.push(format!("{name}: pulled .pm/ state"));
                    }
                    Err(_) => {
                        let _ = git::merge_abort(&pm_dir);
                        let upstream = format!("origin/{branch}");
                        if git::ref_exists(&pm_dir, &upstream)? {
                            git::reset_hard(&pm_dir, &upstream)?;
                            messages.push(format!(
                                "{name}: reset .pm/ to remote state (local and remote diverged)"
                            ));
                        } else {
                            messages.push(format!(
                                "{name}: .pm/ pull failed and no remote branch to reset to"
                            ));
                        }
                    }
                }
            }
        } else {
            messages.push(format!(
                "{name}: .pm/ has no git repo (run `pm state init` in the project)"
            ));
        }
    }

    // Step 3: Open the project (recreate tmux sessions)
    match super::open::open(&root, tmux_server) {
        Ok(result) => {
            if result.sessions_restored > 0 || result.agents_respawned > 0 {
                messages.push(format!(
                    "{name}: restored {} sessions, respawned {} agents",
                    result.sessions_restored, result.agents_respawned
                ));
            } else {
                messages.push(format!("{name}: sessions opened"));
            }
        }
        Err(e) => {
            messages.push(format!("{name}: open failed: {e}"));
        }
    }

    Ok(ProjectResult { messages })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn restore_skips_when_no_repo_url_and_dir_missing() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();

        // Register a project pointing to a non-existent path with no repo_url
        let entry = ProjectEntry {
            root: dir.path().join("nonexistent").to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "ghost").unwrap();

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.contains("skipped (directory does not exist and no repo_url)")),
            "{msgs:?}"
        );
    }

    #[test]
    fn restore_existing_project_opens_sessions() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();
        let name = server.scope("existing");

        // Create a real project via init
        let project_path = dir.path().join(&name);
        super::super::init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Kill the session so open has something to restore
        crate::tmux::kill_session(server.name(), &format!("{name}/main")).unwrap();

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("directory exists")),
            "{msgs:?}"
        );
        // Session should be restored
        assert!(crate::tmux::has_session(server.name(), &format!("{name}/main")).unwrap());
    }

    #[test]
    fn restore_clones_missing_project_with_repo_url() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();
        let name = server.scope("cloned");

        // Create a bare repo to act as the remote
        let bare_path = dir.path().join("remote.git");
        crate::git::init_bare(&bare_path).unwrap();
        // Push an initial commit
        let staging = dir.path().join("staging");
        crate::git::init_repo(&staging).unwrap();
        crate::git::add_remote(&staging, "origin", &bare_path.to_string_lossy()).unwrap();
        crate::git::push(&staging, "origin", "main").unwrap();

        // Register with repo_url but don't create the project dir
        let project_path = dir.path().join(&name);
        let entry = ProjectEntry {
            root: project_path.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: Some(bare_path.to_string_lossy().to_string()),
            state_remote: Some("https://example.com/state.git".to_string()),
        };
        entry.save(&projects_dir, &name).unwrap();

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("cloned and initialised")),
            "{msgs:?}"
        );
        assert!(project_path.join("main").join(".git").exists());

        // Verify URLs were preserved in the registry after init
        let loaded = ProjectEntry::load(&projects_dir, &name).unwrap();
        assert!(loaded.repo_url.is_some());
        assert_eq!(
            loaded.state_remote.as_deref(),
            Some("https://example.com/state.git")
        );
    }

    #[test]
    fn restore_sets_pm_state_remote() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();
        let name = server.scope("withstate");

        // Create a real project
        let project_path = dir.path().join(&name);
        super::super::init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Create a bare repo for the .pm/ state remote
        let state_bare = dir.path().join("state-remote.git");
        crate::git::init_bare(&state_bare).unwrap();

        // Push initial .pm/ state
        let pm_dir = paths::pm_dir(&project_path);
        crate::git::add_remote(&pm_dir, "origin", &state_bare.to_string_lossy()).unwrap();
        let branch = crate::git::current_branch(&pm_dir).unwrap();
        crate::git::push(&pm_dir, "origin", &branch).unwrap();

        // Now remove the remote so restore can set it up
        std::process::Command::new("git")
            .args([
                "-C",
                &pm_dir.to_string_lossy(),
                "remote",
                "remove",
                "origin",
            ])
            .output()
            .unwrap();

        // Update registry entry with state_remote
        let mut entry = ProjectEntry::load(&projects_dir, &name).unwrap();
        entry.state_remote = Some(state_bare.to_string_lossy().to_string());
        entry.save(&projects_dir, &name).unwrap();

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.contains("Set state remote to") && m.contains("and pulled")),
            "{msgs:?}"
        );
        assert!(crate::git::has_remote(&pm_dir, "origin").unwrap());
    }

    #[test]
    fn restore_pulls_pm_state_when_remote_exists_but_no_tracking() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();
        let name = server.scope("notrack");

        // Create a real project
        let project_path = dir.path().join(&name);
        super::super::init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Create a bare repo for the .pm/ state remote
        let state_bare = dir.path().join("state-remote.git");
        crate::git::init_bare(&state_bare).unwrap();

        // Push initial .pm/ state to the bare remote
        let pm_dir = paths::pm_dir(&project_path);
        crate::git::add_remote(&pm_dir, "origin", &state_bare.to_string_lossy()).unwrap();
        let branch = crate::git::current_branch(&pm_dir).unwrap();
        crate::git::push(&pm_dir, "origin", &branch).unwrap();

        // Remove upstream tracking but keep the remote configured.
        // This simulates a previous restore that added the remote but
        // failed before pulling, leaving no tracking branch.
        std::process::Command::new("git")
            .args([
                "-C",
                &pm_dir.to_string_lossy(),
                "config",
                "--unset",
                &format!("branch.{branch}.remote"),
            ])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-C",
                &pm_dir.to_string_lossy(),
                "config",
                "--unset",
                &format!("branch.{branch}.merge"),
            ])
            .output()
            .unwrap();

        // Verify remote exists but tracking does not
        assert!(crate::git::has_remote(&pm_dir, "origin").unwrap());
        assert!(
            crate::git::tracking_branch(&pm_dir, &branch)
                .unwrap()
                .is_none()
        );

        // Update registry entry with state_remote
        let mut entry = ProjectEntry::load(&projects_dir, &name).unwrap();
        entry.state_remote = Some(state_bare.to_string_lossy().to_string());
        entry.save(&projects_dir, &name).unwrap();

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("pulled .pm/ state")),
            "expected 'pulled .pm/ state' message but got: {msgs:?}"
        );
    }

    #[test]
    fn restore_empty_registry() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        let server = TestServer::new();

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        assert!(msgs.iter().any(|m| m.contains("No projects")), "{msgs:?}");
    }
}
