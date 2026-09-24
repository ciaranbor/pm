use std::io::{self, Write};
use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::agent::AgentRegistry;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{gh, git, messages, tmux};

use super::feat_delete::{CleanupParams, check_safety, cleanup_feature};

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

        let report = check_safety(&worktree_path, &main_repo, &state.branch, main_branch)?;

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

/// Delete a project: safety-check all features, kill sessions, remove state and registry.
///
/// Without `--force`, every worktree directory is left in place — `main` holds the
/// repository the feature worktrees link into, so it stays with them. With `--force`,
/// features, `main`, and the then-empty project root are removed from disk; a
/// symlinked `main` (from `pm register` without `--move`) loses only the link.
pub fn delete(
    project_root: &Path,
    projects_dir: &Path,
    force: bool,
    yes: bool,
    tmux_server: Option<&str>,
) -> Result<String> {
    let pm_dir = paths::pm_dir(project_root);
    let features_dir = paths::features_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = config.project.name.clone();

    let features = FeatureState::list(&features_dir)?;
    let main_repo = paths::main_worktree(project_root);

    // --- Safety checks (skip with --force) ---
    if !force && !features.is_empty() {
        let blockers = check_all_features_safety(project_root, &features, "main")?;
        if !blockers.is_empty() {
            let mut msg = String::from("Cannot delete project — the following issues were found:");
            for b in &blockers {
                msg.push_str(&format!("\n  - {b}"));
            }
            msg.push_str("\n\nUse --force to override.");
            return Err(PmError::SafetyCheck(msg));
        }
    }

    // --- Warn about untracked files (unless --force, which removes everything anyway) ---
    if !force {
        for (name, state) in &features {
            let worktree_path = project_root.join(&state.worktree);
            let untracked = git::untracked_files(&worktree_path).unwrap_or_default();
            if !untracked.is_empty() {
                eprintln!(
                    "warning: feature '{name}' has {} untracked file(s):",
                    untracked.len()
                );
                for f in &untracked {
                    eprintln!("  {f}");
                }
            }
        }
    } else if git::has_uncommitted_changes(&main_repo).unwrap_or(false) {
        eprintln!(
            "warning: {} has uncommitted changes; --force removes it",
            main_repo.display()
        );
    }

    // --- Confirmation prompt (skip with --yes) ---
    if !yes {
        let feat_count = features.len();
        let what = if force {
            format!(" and the checkout at {}", main_repo.display())
        } else {
            String::new()
        };
        if feat_count > 0 {
            eprint!("Delete project '{project_name}', its {feat_count} feature(s){what}? [y/N] ");
        } else {
            eprint!("Delete project '{project_name}'{what}? [y/N] ");
        }
        io::stderr().flush()?;

        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            eprintln!("Aborted.");
            return Ok(project_name);
        }
    }

    // --- Delete all features ---
    for (name, state) in &features {
        let worktree_path = project_root.join(&state.worktree);

        if force {
            // --force: full cleanup including worktree directory removal
            cleanup_feature(&CleanupParams {
                repo: &main_repo,
                worktree_path: &worktree_path,
                branch: &state.branch,
                features_dir: &features_dir,
                name,
                project_name: &project_name,
                force_worktree: true,
                tmux_server,
                delete_branch: true,
                best_effort: false,
                base: state.base_or_default(),
            })?;
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

            let session_name = tmux::session_name(&project_name, name);
            if tmux::has_session(tmux_server, &session_name)? {
                let main_session = tmux::session_name(&project_name, "main");
                let _ = tmux::switch_client(tmux_server, &main_session);
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

    // --- Kill main tmux session (must be last — if the caller is inside this
    // session, the kill terminates this process) ---
    let main_session = tmux::session_name(&project_name, "main");
    if tmux::has_session(tmux_server, &main_session)? {
        tmux::kill_session(tmux_server, &main_session)?;
    }

    Ok(project_name)
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
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn delete_empty_project_removes_all_resources() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) = server.setup_project(dir.path());
        let main_session = tmux::session_name(&project_name, "main");

        assert!(tmux::has_session(server.name(), &main_session).unwrap());

        delete(&project_path, &projects_dir, false, true, server.name()).unwrap();

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

        delete(&project_path, &projects_dir, false, true, server.name()).unwrap();

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
            "login",
            server.name(),
        ))
        .unwrap();

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("dirty.txt"), "uncommitted").unwrap();
        git::stage_file(&worktree, "dirty.txt").unwrap();

        let result = delete(&project_path, &projects_dir, false, true, server.name());
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
            "login",
            server.name(),
        ))
        .unwrap();

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("feature.txt"), "content").unwrap();
        git::stage_file(&worktree, "feature.txt").unwrap();
        git::commit(&worktree, "feature work").unwrap();

        let result = delete(&project_path, &projects_dir, false, true, server.name());
        assert!(result.is_err());

        assert!(paths::pm_dir(&project_path).exists());
    }

    #[test]
    fn delete_force_bypasses_safety_checks_and_removes_worktrees() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        let worktree = project_path.join("login");
        std::fs::write(worktree.join("dirty.txt"), "uncommitted").unwrap();
        git::stage_file(&worktree, "dirty.txt").unwrap();

        delete(&project_path, &projects_dir, true, true, server.name()).unwrap();

        assert!(!paths::pm_dir(&project_path).exists());
        assert!(!projects_dir.join(format!("{project_name}.toml")).exists());
        // --force removes every worktree, main included, and the emptied root
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

        delete(&project_path, &projects_dir, true, true, server.name()).unwrap();

        assert!(!project_path.exists());
        assert!(real_repo.join(".git").exists());
    }

    #[test]
    fn delete_keeps_a_root_holding_user_files() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        std::fs::write(project_path.join("notes.txt"), "mine").unwrap();

        delete(&project_path, &projects_dir, true, true, server.name()).unwrap();

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
            "login",
            server.name(),
        ))
        .unwrap();

        // Merge the feature branch into main so safety checks pass
        let main_repo = paths::main_worktree(&project_path);
        git::merge_no_ff(&main_repo, "login").unwrap();

        delete(&project_path, &projects_dir, false, true, server.name()).unwrap();

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

        let result = delete(&project_path, &projects_dir, false, true, None);
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
            "clean",
            server.name(),
        ))
        .unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "dirty",
            server.name(),
        ))
        .unwrap();

        let main_repo = paths::main_worktree(&project_path);
        git::merge_no_ff(&main_repo, "clean").unwrap();

        let worktree = project_path.join("dirty");
        std::fs::write(worktree.join("file.txt"), "content").unwrap();
        git::stage_file(&worktree, "file.txt").unwrap();

        let result = delete(&project_path, &projects_dir, false, true, server.name());
        assert!(result.is_err());

        // Nothing should have been deleted
        assert!(paths::pm_dir(&project_path).exists());
    }
}
