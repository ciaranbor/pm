use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::agent::AgentRegistry;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};
use crate::{gh, git, messages, tmux};

use super::feat_delete::{CleanupParams, base_scope, check_safety, cleanup_feature};

/// Collect safety problems across all features. Returns a list of blocking messages.
fn check_all_features_safety(
    project_root: &Path,
    features: &[(String, FeatureState)],
    main_branch: &str,
) -> Result<Vec<String>> {
    let main_repo = paths::main_worktree(project_root);
    let mut blockers = Vec::new();

    for (name, state) in features {
        let worktree_path = project_root.join(&state.worktree);
        if !worktree_path.exists() {
            continue;
        }

        let live = git::is_worktree(&main_repo, &worktree_path)?;
        let report = check_safety(
            live.then_some(worktree_path.as_path()),
            &main_repo,
            &state.branch,
            main_branch,
        )?;

        let pr_merged =
            !state.pr.is_empty() && gh::pr_is_merged(&main_repo, &state.pr).unwrap_or(false);

        if report.has_uncommitted_changes {
            blockers.push(format!("feature '{name}' has uncommitted changes"));
        }

        if !report.is_merged && !pr_merged {
            blockers.push(format!(
                "feature '{name}' has commits not merged into {main_branch}"
            ));
        } else if report.has_unpushed_commits && !pr_merged {
            // Only check unpushed when the branch is merged — an unmerged branch
            // already implies the commits aren't where they need to be.
            blockers.push(format!("feature '{name}' has unpushed commits"));
        }
    }

    Ok(blockers)
}

/// What a delete is about to remove, for its confirmation.
pub struct Pending<'a> {
    pub project: &'a str,
    pub features: usize,
    /// The main checkout, which only `--force` removes.
    pub main: &'a Path,
    /// What the CLI warns of before it asks: untracked files in feature
    /// worktrees, and with `--force`, what only `main` holds.
    pub warnings: &'a [String],
}

/// What [`delete`] did.
pub struct Deleted {
    pub project: String,
    /// What of a `--force` teardown could not be removed.
    pub warnings: Vec<String>,
    /// The session this process runs in, when it was one of the project's.
    pub own: Option<tmux::OwnSession>,
}

/// Delete a project: safety-check all features, kill sessions, remove state and registry.
/// `confirm` is asked once the checks pass and before anything is removed;
/// `None` when it declined.
///
/// Without `--force`, every worktree directory is left in place — `main` holds the
/// repository the feature worktrees link into, so it stays with them. With `--force`,
/// features, `main`, and the then-empty project root are removed from disk; a
/// symlinked `main` (from `pm register` without `--move`) loses only the link.
pub fn delete(
    project_root: &Path,
    projects_dir: &Path,
    force: bool,
    tmux_server: Option<&str>,
    confirm: impl FnOnce(&Pending) -> Result<bool>,
) -> Result<Option<Deleted>> {
    let pm_dir = paths::pm_dir(project_root);
    let features_dir = paths::features_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = config.project.name.clone();

    let features = FeatureState::list(&features_dir)?;
    let main_repo = paths::main_worktree(project_root);
    let main_branch = ProjectEntry::load(projects_dir, &project_name)?.main_branch;

    if !force && !features.is_empty() {
        let blockers = check_all_features_safety(project_root, &features, &main_branch)?;
        if !blockers.is_empty() {
            let mut reason =
                String::from("Cannot delete project — the following issues were found:");
            for b in &blockers {
                reason.push_str(&format!("\n  - {b}"));
            }
            return Err(PmError::Unsafe {
                reason,
                cli: "\n\nUse --force to override.".to_string(),
                remote: format!(
                    "\n\n`pm delete --force --project {project_name}` at a terminal deletes it anyway, \
                     along with its worktrees and the main checkout."
                ),
            });
        }
    }

    let warnings = if force {
        force_loss_warnings(&main_repo)
    } else {
        features
            .iter()
            .filter_map(|(name, state)| {
                let untracked =
                    git::untracked_files(&project_root.join(&state.worktree)).unwrap_or_default();
                (!untracked.is_empty()).then(|| {
                    format!(
                        "feature '{name}' has {} untracked file(s): {}",
                        untracked.len(),
                        listed(&untracked)
                    )
                })
            })
            .collect()
    };

    let pending = Pending {
        project: &project_name,
        features: features.len(),
        main: &main_repo,
        warnings: &warnings,
    };
    if !confirm(&pending)? {
        return Ok(None);
    }
    let mut warnings = Vec::new();

    // --- Delete all features ---
    let own = tmux::own_session(tmux_server)
        .filter(|own| {
            features
                .iter()
                .map(|(name, _)| name.as_str())
                .chain(["main"])
                .any(|scope| tmux::session_name(&project_name, scope) == *own)
        })
        .map(|name| tmux::OwnSession {
            name,
            preferred: None,
        });
    for (name, state) in &features {
        let worktree_path = project_root.join(&state.worktree);
        let session_name = tmux::session_name(&project_name, name);
        let kill_session = own.as_ref().is_none_or(|own| own.name != session_name);

        if force {
            // --force: full cleanup including worktree directory removal
            let cleanup = cleanup_feature(&CleanupParams {
                repo: &main_repo,
                worktree_path: &worktree_path,
                branch: &state.branch,
                features_dir: &features_dir,
                name,
                project_name: &project_name,
                force_worktree: true,
                worktree_created: true,
                tmux_server,
                kill_session,
                delete_branch: true,
                best_effort: false,
                base_scope: &base_scope(
                    project_root,
                    &main_branch,
                    state.base_branch(&main_branch),
                ),
                ending: None,
            })?;
            warnings.extend(cleanup);
        } else {
            // Soft teardown: remove pm state and tmux session, but leave
            // the worktree directories and git branches intact so the user
            // can still use them as plain git repos.
            FeatureState::delete(&features_dir, name)?;

            // Clean up per-feature agent registry and message queue
            let agents_dir = paths::agents_dir(project_root);
            AgentRegistry::delete(&agents_dir, name)?;
            let messages_dir = paths::messages_dir(project_root);
            messages::delete_feature(&messages_dir, name)?;

            if kill_session && tmux::has_session(tmux_server, &session_name)? {
                let main_session = tmux::session_name(&project_name, "main");
                tmux::clients::move_off(
                    tmux_server,
                    std::slice::from_ref(&session_name),
                    Some(&main_session),
                )?;
                tmux::kill_session(tmux_server, &session_name)?;
            }
        }
    }

    // --- Remove .pm/ directory ---
    if pm_dir.exists() {
        std::fs::remove_dir_all(&pm_dir)?;
    }

    // --- Remove global registry entry ---
    let registry_file = projects_dir.join(format!("{project_name}.toml"));
    if registry_file.exists() {
        std::fs::remove_file(&registry_file)?;
    }

    // --- Remove the main checkout and the project root (--force only) ---
    if force {
        remove_main_checkout(&main_repo)?;
        // Only an empty root is pm's to remove: anything else in it is the user's.
        let _ = std::fs::remove_dir(project_root);
    }

    let main_session = tmux::session_name(&project_name, "main");
    if own.as_ref().is_none_or(|own| own.name != main_session)
        && tmux::has_session(tmux_server, &main_session)?
    {
        tmux::clients::move_off(tmux_server, std::slice::from_ref(&main_session), None)?;
        tmux::kill_session(tmux_server, &main_session)?;
    }

    Ok(Some(Deleted {
        project: project_name,
        warnings,
        own,
    }))
}

/// The first few of `files`, and how many more there are.
fn listed(files: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut out = files[..files.len().min(SHOWN)].join(", ");
    if files.len() > SHOWN {
        out.push_str(&format!(" and {} more", files.len() - SHOWN));
    }
    out
}

/// What `--force` would destroy along with `main` that exists nowhere else:
/// the whole history when the repository has no remote, commits its branch
/// has not pushed, and uncommitted changes.
fn force_loss_warnings(main_repo: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let shown = main_repo.display();
    if git::list_remotes(main_repo)
        .map(|r| r.trim().is_empty())
        .unwrap_or(false)
    {
        out.push(format!(
            "{shown} has no remote; --force deletes its only copy of the history"
        ));
    } else if git::has_unpushed_commits(main_repo).unwrap_or(false) {
        out.push(format!(
            "{shown} has unpushed commits; --force deletes them"
        ));
    }
    if git::has_uncommitted_changes(main_repo).unwrap_or(false) {
        out.push(format!(
            "{shown} has uncommitted changes; --force deletes them"
        ));
    }
    out
}

/// Remove `main` from disk. A symlinked `main` points at a repository pm never
/// owned, so only the link goes.
fn remove_main_checkout(main_repo: &Path) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(main_repo) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        std::fs::remove_file(main_repo)?;
    } else {
        std::fs::remove_dir_all(main_repo)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feat_new;
    use crate::testing::{ControlClient, OwnServer, TestServer};
    use tempfile::tempdir;

    #[test]
    fn delete_moves_only_the_clients_viewing_the_project_off_it() {
        let dir = tempdir().unwrap();
        let (project, name) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        let own = OwnServer::start("delete-clients");
        let login = tmux::session_name(&name, "login");
        for session in ["elsewhere", &tmux::session_name(&name, "main"), &login] {
            tmux::create_session(own.name(), session, dir.path()).unwrap();
        }
        // The bystander attaches last, so it is the client tmux takes to be
        // current.
        let _viewer = ControlClient::attach(own.name(), &login);
        let _bystander = ControlClient::attach(own.name(), "elsewhere");
        let clients = tmux::clients::list(own.name()).unwrap();
        let bystander = clients.iter().find(|c| c.session == "elsewhere").unwrap();
        let bystander = bystander.name.clone();

        let projects_dir = TestServer::registry_dir(&project);
        delete(&project, &projects_dir, false, own.name(), |_| Ok(true)).unwrap();

        let clients = tmux::clients::list(own.name()).unwrap();
        assert_eq!(clients.len(), 2, "a client was detached: {clients:?}");
        let stayed = clients.iter().find(|c| c.name == bystander).unwrap();
        assert_eq!(stayed.session, "elsewhere");
        assert!(clients.iter().all(|c| !c.session.starts_with(&name)));
    }

    #[test]
    fn delete_empty_project_removes_all_resources() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) = server.setup_project(dir.path());
        let main_session = tmux::session_name(&project_name, "main");

        assert!(tmux::has_session(server.name(), &main_session).unwrap());

        delete(&project_path, &projects_dir, false, server.name(), |_| {
            Ok(true)
        })
        .unwrap();

        assert!(!paths::pm_dir(&project_path).exists());
        assert!(!projects_dir.join(format!("{project_name}.toml")).exists());
        assert!(!tmux::has_session(server.name(), &main_session).unwrap());
    }

    #[test]
    fn delete_cleans_up_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "api",
            server.name(),
        ))
        .unwrap();

        delete(&project_path, &projects_dir, false, server.name(), |_| {
            Ok(true)
        })
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
        assert!(!FeatureState::exists(&features_dir, "api"));
        assert!(
            !tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
        assert!(
            !tmux::has_session(server.name(), &tmux::session_name(&project_name, "api")).unwrap()
        );
        assert!(!paths::pm_dir(&project_path).exists());
    }

    #[test]
    fn delete_blocked_by_uncommitted_changes() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("dirty.txt"), "uncommitted").unwrap();
        git::stage_file(&worktree, "dirty.txt").unwrap();

        let result = delete(&project_path, &projects_dir, false, server.name(), |_| {
            Ok(true)
        });
        assert!(result.is_err());

        // Everything should still exist
        assert!(paths::pm_dir(&project_path).exists());
        assert!(projects_dir.join(format!("{project_name}.toml")).exists());
    }

    #[test]
    fn delete_blocked_by_unmerged_commits() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _project_name) = server.setup_project(dir.path());

        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("feature.txt"), "content").unwrap();
        git::stage_file(&worktree, "feature.txt").unwrap();
        git::commit(&worktree, "feature work").unwrap();

        let result = delete(&project_path, &projects_dir, false, server.name(), |_| {
            Ok(true)
        });
        assert!(result.is_err());

        assert!(paths::pm_dir(&project_path).exists());
    }

    #[test]
    fn delete_checks_merges_against_the_registered_main_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_master_project(dir.path());
        let main = paths::main_worktree(&project_path);

        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();
        let worktree = project_path.join("login");
        std::fs::write(worktree.join("feature.txt"), "content").unwrap();
        git::stage_file(&worktree, "feature.txt").unwrap();
        git::commit(&worktree, "feature work").unwrap();
        git::merge_no_ff(&main, "login").unwrap();

        delete(&project_path, &projects_dir, false, server.name(), |_| {
            Ok(true)
        })
        .unwrap();

        assert!(!paths::pm_dir(&project_path).exists());
    }

    #[test]
    fn delete_force_bypasses_safety_checks_and_removes_worktrees() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("dirty.txt"), "uncommitted").unwrap();
        git::stage_file(&worktree, "dirty.txt").unwrap();

        delete(&project_path, &projects_dir, true, server.name(), |_| {
            Ok(true)
        })
        .unwrap();

        assert!(!paths::pm_dir(&project_path).exists());
        assert!(!projects_dir.join(format!("{project_name}.toml")).exists());
        assert!(!project_path.join("login").exists());
        assert!(!paths::main_worktree(&project_path).exists());
        assert!(!project_path.exists());
    }

    #[test]
    fn delete_force_removes_only_the_link_of_a_symlinked_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());

        // Stand in for `pm register` symlink mode: main → a repo pm doesn't own.
        let main = paths::main_worktree(&project_path);
        let real_repo = dir.path().join("real-repo");
        std::fs::rename(&main, &real_repo).unwrap();
        std::os::unix::fs::symlink(&real_repo, &main).unwrap();

        delete(&project_path, &projects_dir, true, server.name(), |_| {
            Ok(true)
        })
        .unwrap();

        assert!(!project_path.exists());
        assert!(real_repo.join(".git").exists());
    }

    #[test]
    fn a_long_list_of_untracked_files_is_cut_to_the_first_few() {
        let files: Vec<String> = (1..=7).map(|i| format!("f{i}")).collect();
        assert_eq!(listed(&files[..2]), "f1, f2");
        assert_eq!(listed(&files), "f1, f2, f3, f4, f5 and 2 more");
    }

    #[test]
    fn force_warns_about_history_only_main_holds() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        let main = paths::main_worktree(&project_path);

        let warnings = force_loss_warnings(&main);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("no remote"), "{warnings:?}");

        std::fs::write(main.join("wip.txt"), "wip").unwrap();
        git::stage_file(&main, "wip.txt").unwrap();
        let warnings = force_loss_warnings(&main);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[1].contains("uncommitted changes"), "{warnings:?}");
    }

    #[test]
    fn delete_keeps_a_root_holding_user_files() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        std::fs::write(project_path.join("notes.txt"), "mine").unwrap();

        delete(&project_path, &projects_dir, true, server.name(), |_| {
            Ok(true)
        })
        .unwrap();

        assert!(!paths::main_worktree(&project_path).exists());
        assert!(project_path.join("notes.txt").exists());
    }

    #[test]
    fn delete_merged_features_succeeds_but_leaves_worktrees_on_disk() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();

        // Merge the feature branch into main so safety checks pass
        let main_repo = paths::main_worktree(&project_path);
        git::merge_no_ff(&main_repo, "login").unwrap();

        delete(&project_path, &projects_dir, false, server.name(), |_| {
            Ok(true)
        })
        .unwrap();

        // pm state and registry are cleaned up
        assert!(!paths::pm_dir(&project_path).exists());
        assert!(!projects_dir.join(format!("{project_name}.toml")).exists());
        // Without --force, worktree directories and branches are left on disk;
        // the feature worktree is only usable while main's repository stays.
        assert!(project_path.join("login").exists());
        assert!(main_repo.join(".git").exists());
        assert!(git::branch_exists(&main_repo, "login").unwrap());
    }

    #[test]
    fn delete_nonexistent_project_fails() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("noproject");
        let projects_dir = dir.path().join("registry");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let result = delete(&project_path, &projects_dir, false, None, |_| Ok(true));
        assert!(result.is_err());
    }

    #[test]
    fn delete_only_blocks_for_problematic_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _project_name) = server.setup_project(dir.path());

        // Create two features — one clean (merged), one dirty
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "clean",
            server.name(),
        ))
        .unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "dirty",
            server.name(),
        ))
        .unwrap();

        let main_repo = paths::main_worktree(&project_path);
        git::merge_no_ff(&main_repo, "clean").unwrap();

        let worktree = project_path.join("dirty");
        std::fs::write(worktree.join("file.txt"), "content").unwrap();
        git::stage_file(&worktree, "file.txt").unwrap();

        let result = delete(&project_path, &projects_dir, false, server.name(), |_| {
            Ok(true)
        });
        assert!(result.is_err());

        // Nothing should have been deleted
        assert!(paths::pm_dir(&project_path).exists());
    }
}
