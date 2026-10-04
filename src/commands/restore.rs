//! `pm restore`: rebuild registered projects on a fresh machine from the
//! global registry. Each project is cloned, its `.pm/` state pulled, and its
//! active features' worktrees recreated before any agent starts; sessions
//! from `--import` tarballs go in next, so that `pm open` resumes every
//! agent's conversation instead of starting it before the conversation
//! exists here.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::git;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectEntry};

/// What to restore, and from where.
pub struct RestoreParams<'a> {
    pub projects_dir: &'a Path,
    /// Registered names to restore; empty restores every one.
    pub projects: &'a [String],
    /// `pm harness export` tarballs to import before agents start.
    pub imports: &'a [PathBuf],
    pub home: &'a Path,
    pub global: &'a GlobalConfig,
    pub tmux_server: Option<&'a str>,
}

/// Restore projects from the global registry on a fresh machine.
pub fn restore(
    projects: &[String],
    imports: &[PathBuf],
    tmux_server: Option<&str>,
) -> Result<Vec<String>> {
    let projects_dir = paths::global_projects_dir()?;
    restore_with(&RestoreParams {
        projects_dir: &projects_dir,
        projects,
        imports,
        home: &paths::home_dir()?,
        global: &GlobalConfig::load_or_default(),
        tmux_server,
    })
}

/// Testable inner function that takes an explicit projects directory.
#[cfg(test)]
fn restore_with_dir(projects_dir: &Path, tmux_server: Option<&str>) -> Result<Vec<String>> {
    restore_with(&RestoreParams {
        projects_dir,
        projects: &[],
        imports: &[],
        home: &paths::home_dir()?,
        global: &GlobalConfig::default(),
        tmux_server,
    })
}

pub fn restore_with(params: &RestoreParams<'_>) -> Result<Vec<String>> {
    let mut projects = ProjectEntry::list(params.projects_dir)?;
    if !params.projects.is_empty() {
        let unknown: Vec<&str> = params
            .projects
            .iter()
            .filter(|name| !projects.iter().any(|(n, _)| n == *name))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            return Err(PmError::ProjectNotFound(unknown.join(", ")));
        }
        projects.retain(|(name, _)| params.projects.contains(name));
    }
    for tarball in params.imports {
        if !tarball.is_file() {
            return Err(PmError::ExportImport(format!(
                "tarball not found: {}",
                tarball.display()
            )));
        }
    }

    if projects.is_empty() {
        return Ok(vec!["No projects in registry".to_string()]);
    }

    let mut all_messages = Vec::new();
    let mut restored = Vec::new();
    for (name, entry) in &projects {
        match restore_project(name, entry, params.projects_dir, params.tmux_server) {
            Ok(pr) => {
                all_messages.extend(pr.messages);
                if pr.ready {
                    restored.push((name, entry.root_path()));
                }
            }
            Err(e) => all_messages.push(format!("{name}: error: {e}")),
        }
    }

    // Printed last, where they are not lost among the rest.
    let mut warnings = Vec::new();
    for tarball in params.imports {
        match super::harness_import::import(
            None,
            tarball,
            params.projects,
            params.projects_dir,
            params.home,
            &params.global.harness,
        ) {
            Ok(report) => {
                all_messages.extend(report.messages);
                warnings.extend(report.missed.iter().map(|missed| {
                    format!(
                        "warning: sessions of {missed} in {} were not imported: \
                         its agents will not resume them",
                        tarball.display()
                    )
                }));
            }
            Err(e) => warnings.push(format!(
                "warning: {}: import failed: {e}",
                tarball.display()
            )),
        }
    }

    for (name, root) in &restored {
        all_messages.push(open_project(
            name,
            root,
            params.projects_dir,
            params.tmux_server,
        ));
    }
    all_messages.extend(warnings);

    Ok(all_messages)
}

/// Result of restoring a single project's repo, state and worktrees.
struct ProjectResult {
    messages: Vec<String>,
    /// Whether the project is on disk, so its sessions can be opened.
    ready: bool,
}

/// Recreate a project's tmux sessions and respawn its active agents.
fn open_project(name: &str, root: &Path, projects_dir: &Path, tmux_server: Option<&str>) -> String {
    match super::open::open(root, projects_dir, tmux_server) {
        Ok(result) if result.sessions_restored > 0 || result.agents_respawned > 0 => format!(
            "{name}: restored {} sessions, respawned {} agents",
            result.sessions_restored, result.agents_respawned
        ),
        Ok(_) => format!("{name}: sessions opened"),
        Err(e) => format!("{name}: open failed: {e}"),
    }
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
            return Ok(ProjectResult {
                messages,
                ready: false,
            });
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
        } else if pm_dir.exists() {
            // .pm/ exists but isn't a git repo (e.g. registered via `pm register --move`).
            // Initialise it and pull from the remote.
            match super::state_cmd::init_with_remote(&root, Some(state_remote_url)) {
                Ok(msg) => {
                    messages.push(format!("{name}: {msg}"));
                }
                Err(e) => {
                    messages.push(format!("{name}: .pm/ state init failed: {e}"));
                }
            }
        } else {
            messages.push(format!(
                "{name}: .pm/ does not exist (run `pm init` in the project)"
            ));
        }
    }

    // Step 2.5: Recreate missing feature worktrees
    let features_dir = paths::features_dir(&root);
    let main_worktree = paths::main_worktree(&root);
    if main_worktree.exists() {
        // Harness projections are not tracked, so a clone has none.
        if let Err(e) = super::skills::project_assets(&root, false) {
            messages.push(format!("{name}: warning: could not project assets: {e}"));
        }
        match FeatureState::list(&features_dir) {
            Ok(features) => {
                for (feat_name, feat_state) in &features {
                    if !feat_state.status.is_active() {
                        continue;
                    }
                    let wt_path = root.join(&feat_state.worktree);
                    if wt_path.exists() {
                        continue;
                    }
                    // git worktree add has DWIM mode: if the local branch
                    // doesn't exist but origin/<branch> does (common after
                    // clone on a fresh machine), git auto-creates the local
                    // branch from the remote tracking ref.
                    match git::add_worktree(&main_worktree, &wt_path, &feat_state.branch) {
                        Ok(()) => {
                            messages.push(format!(
                                "{name}: recreated worktree for feature '{feat_name}'"
                            ));
                            if let Err(e) = super::seed::seed_feature_assets(&root, &wt_path) {
                                messages.push(format!(
                                    "{name}: warning: could not seed '{feat_name}': {e}"
                                ));
                            }
                        }
                        Err(e) => {
                            messages.push(format!(
                                "{name}: warning: failed to recreate worktree for '{feat_name}': {e}"
                            ));
                        }
                    }
                }
            }
            Err(e) => {
                messages.push(format!("{name}: warning: could not list features: {e}"));
            }
        }
    }

    Ok(ProjectResult {
        messages,
        ready: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use crate::tmux;
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
        crate::tmux::kill_session(server.name(), &tmux::session_name(&name, "main")).unwrap();

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("directory exists")),
            "{msgs:?}"
        );
        // Session should be restored
        assert!(
            crate::tmux::has_session(server.name(), &tmux::session_name(&name, "main")).unwrap()
        );
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
        assert!(paths::main_worktree(&project_path).join(".git").exists());

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
    fn restore_inits_pm_state_when_no_git_repo() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();
        let name = server.scope("nogit");

        // Create a real project so .pm/ exists with a .git
        let project_path = dir.path().join(&name);
        super::super::init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        let pm_dir = paths::pm_dir(&project_path);

        // Create a bare repo for the state remote and push .pm/ state to it
        let state_bare = dir.path().join("state-remote.git");
        crate::git::init_bare(&state_bare).unwrap();
        crate::git::add_remote(&pm_dir, "origin", &state_bare.to_string_lossy()).unwrap();
        let branch = crate::git::current_branch(&pm_dir).unwrap();
        crate::git::push(&pm_dir, "origin", &branch).unwrap();

        // Remove .pm/.git to simulate a `pm register --move` scenario
        std::fs::remove_dir_all(pm_dir.join(".git")).unwrap();
        assert!(!pm_dir.join(".git").exists());
        assert!(pm_dir.exists());

        // Update registry entry with state_remote
        let mut entry = ProjectEntry::load(&projects_dir, &name).unwrap();
        entry.state_remote = Some(state_bare.to_string_lossy().to_string());
        entry.save(&projects_dir, &name).unwrap();

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("Initialised state repo")),
            "expected state repo init message but got: {msgs:?}"
        );
        // .pm/.git should now exist
        assert!(pm_dir.join(".git").exists());
    }

    #[test]
    fn restore_recreates_missing_feature_worktrees() {
        use crate::state::feature::{FeatureState, FeatureStatus};
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();
        let name = server.scope("wtrec");

        // Create a real project via init
        let project_path = dir.path().join(&name);
        super::super::init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        let main_worktree = paths::main_worktree(&project_path);
        let skill = main_worktree.join(".agents/skills/demo");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: demo\n---\n").unwrap();
        git::add_all(&main_worktree).unwrap();
        git::commit_with_message(&main_worktree, "demo skill").unwrap();

        // Create a feature branch and worktree
        git::create_branch(&main_worktree, "feat-login").unwrap();
        let wt_path = project_path.join("login");
        git::add_worktree(&main_worktree, &wt_path, "feat-login").unwrap();
        assert!(wt_path.exists());

        // Register feature state in .pm/features/
        let features_dir = paths::features_dir(&project_path);
        let now = Utc::now();
        let feat_state = FeatureState {
            status: FeatureStatus::Wip,
            branch: "feat-login".to_string(),
            worktree: "login".to_string(),
            base: "main".to_string(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: now,
            last_active: now,
            progress: Default::default(),
            blocked_reason: None,
            blocked_by: None,
        };
        feat_state.save(&features_dir, "login").unwrap();

        // Remove the worktree directory to simulate a fresh machine
        git::remove_worktree(&main_worktree, &wt_path).unwrap();
        assert!(!wt_path.exists());

        // Kill the session so open doesn't complain
        let _ = crate::tmux::kill_session(server.name(), &tmux::session_name(&name, "main"));

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.contains("recreated worktree for feature 'login'")),
            "expected worktree recreation message but got: {msgs:?}"
        );
        // Worktree directory should exist again, with the harness
        // projection a new feature gets.
        assert!(wt_path.exists());
        assert!(wt_path.join(".claude/skills/demo/SKILL.md").exists());
    }

    #[test]
    fn restore_only_the_named_projects() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();
        for name in ["wanted", "other"] {
            ProjectEntry {
                root: dir.path().join(name).to_string_lossy().to_string(),
                main_branch: "main".to_string(),
                repo_url: None,
                state_remote: None,
            }
            .save(&projects_dir, name)
            .unwrap();
        }
        let global = GlobalConfig::default();
        let restore = |names: &[&str]| {
            let projects: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            restore_with(&RestoreParams {
                projects_dir: &projects_dir,
                projects: &projects,
                imports: &[],
                home: dir.path(),
                global: &global,
                tmux_server: server.name(),
            })
        };

        assert_eq!(
            restore(&["wanted"]).unwrap(),
            ["wanted: skipped (directory does not exist and no repo_url)"]
        );
        assert!(restore(&["nope"]).is_err());
    }

    #[test]
    fn restore_skips_worktree_for_merged_feature() {
        use crate::state::feature::{FeatureState, FeatureStatus};
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();
        let name = server.scope("wtmerged");

        let project_path = dir.path().join(&name);
        super::super::init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        let main_worktree = paths::main_worktree(&project_path);
        git::create_branch(&main_worktree, "feat-old").unwrap();

        // Register a merged feature — its worktree should NOT be recreated
        let features_dir = paths::features_dir(&project_path);
        let now = Utc::now();
        let feat_state = FeatureState {
            status: FeatureStatus::Merged,
            branch: "feat-old".to_string(),
            worktree: "old-feat".to_string(),
            base: "main".to_string(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: now,
            last_active: now,
            progress: Default::default(),
            blocked_reason: None,
            blocked_by: None,
        };
        feat_state.save(&features_dir, "old-feat").unwrap();

        let _ = crate::tmux::kill_session(server.name(), &tmux::session_name(&name, "main"));

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        // Should NOT contain any worktree recreation message
        assert!(
            !msgs.iter().any(|m| m.contains("recreated worktree")),
            "merged feature worktree should not be recreated: {msgs:?}"
        );
        // Worktree directory should not exist
        assert!(!project_path.join("old-feat").exists());
    }

    #[test]
    fn restore_warns_on_missing_branch() {
        use crate::state::feature::{FeatureState, FeatureStatus};
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();
        let name = server.scope("wtmissing");

        let project_path = dir.path().join(&name);
        super::super::init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Register a feature with a branch that doesn't exist
        let features_dir = paths::features_dir(&project_path);
        let now = Utc::now();
        let feat_state = FeatureState {
            status: FeatureStatus::Wip,
            branch: "nonexistent-branch".to_string(),
            worktree: "ghost-feat".to_string(),
            base: "main".to_string(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: now,
            last_active: now,
            progress: Default::default(),
            blocked_reason: None,
            blocked_by: None,
        };
        feat_state.save(&features_dir, "ghost-feat").unwrap();

        let _ = crate::tmux::kill_session(server.name(), &tmux::session_name(&name, "main"));

        let msgs = restore_with_dir(&projects_dir, server.name()).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.contains("warning: failed to recreate worktree for 'ghost-feat'")),
            "expected warning for missing branch but got: {msgs:?}"
        );
    }

    #[test]
    fn restore_lists_sessions_it_could_not_import_last() {
        use crate::commands::harness_export::tests::{
            run_export, setup_claude_sessions, setup_project,
        };

        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let server = TestServer::new();
        let name = server.scope("importwarn");
        let project_path = dir.path().join(&name);
        super::super::init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        let _ = crate::tmux::kill_session(server.name(), &tmux::session_name(&name, "main"));

        // Exported from a machine where `ghost` is registered; it is not here.
        let source = tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("ghost")).unwrap();
        let source_main = setup_project(
            &source.path().join("ghost"),
            "ghost",
            &source.path().join("registry"),
        );
        setup_claude_sessions(&source.path().join("home"), &source_main);
        let (tarball, _) = run_export(
            crate::harness::Harness::ClaudeCode,
            None,
            &source.path().join("registry"),
            &source.path().join("export.tar.gz"),
            &source.path().join("home"),
        )
        .unwrap();
        let home = tempdir().unwrap();

        let msgs = restore_with(&RestoreParams {
            projects_dir: &projects_dir,
            projects: &[],
            imports: std::slice::from_ref(&tarball),
            home: home.path(),
            global: &GlobalConfig::default(),
            tmux_server: server.name(),
        })
        .unwrap();

        assert!(
            msgs.iter()
                .any(|m| m.starts_with(&format!("{name}: restored"))),
            "{msgs:?}"
        );
        assert_eq!(
            msgs.last().unwrap(),
            &format!(
                "warning: sessions of 'ghost' (not registered locally) in {} were not \
                 imported: its agents will not resume them",
                tarball.display()
            )
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
