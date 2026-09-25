//! `pm feat rebase`: move a feature onto another branch and record that
//! branch as its base.
//!
//! The rebase runs in the feature's own worktree, and the recorded `base`
//! changes only once git reports the branch is on the target. A conflict
//! leaves git's rebase in progress there for the user to finish with git;
//! re-running the command afterwards finds nothing left to replay and
//! records the base. pm keeps no state of its own about a half-done rebase.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;
use crate::state::agent::AgentRegistry;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};

/// Rebase feature `name` onto `onto` (its current base when `None`) and
/// record the branch as the feature's base. Returns the branch recorded.
pub fn feat_rebase(
    project_root: &Path,
    projects_dir: &Path,
    name: &str,
    onto: Option<&str>,
) -> Result<String> {
    let features_dir = paths::features_dir(project_root);
    let mut state = FeatureState::load(&features_dir, name)?;
    let project_name = ProjectConfig::load(&paths::pm_dir(project_root))?
        .project
        .name;
    let main_branch = ProjectEntry::load(projects_dir, &project_name)?.main_branch;

    let target = onto.unwrap_or(state.base_branch(&main_branch)).to_string();
    let worktree = project_root.join(&state.worktree);

    if target == state.branch {
        return Err(PmError::SafetyCheck(format!(
            "cannot rebase feature '{name}' onto its own branch '{target}'"
        )));
    }
    if !git::branch_exists(&worktree, &target)? {
        return Err(PmError::BranchNotFound(target));
    }
    if git::rebase_in_progress(&worktree)? {
        return Err(PmError::SafetyCheck(format!(
            "feature '{name}' has a rebase in progress in {}: finish it with \
             `git rebase --continue` (or `git rebase --abort`), then re-run",
            worktree.display()
        )));
    }
    match git::head_branch(&worktree) {
        Ok(head) if head == state.branch => {}
        Ok(head) => {
            return Err(PmError::SafetyCheck(format!(
                "feature '{name}' worktree has '{head}' checked out, not '{}' — \
                 check the feature branch out before rebasing",
                state.branch
            )));
        }
        Err(_) => {
            return Err(PmError::SafetyCheck(format!(
                "feature '{name}' worktree has a detached HEAD — check out '{}' before rebasing",
                state.branch
            )));
        }
    }
    if git::has_uncommitted_changes(&worktree)? {
        return Err(PmError::SafetyCheck(format!(
            "feature '{name}' has uncommitted changes — commit or stash before rebasing"
        )));
    }

    let registry = AgentRegistry::load(&paths::agents_dir(project_root), name)?;
    let running: Vec<&str> = registry
        .agents
        .iter()
        .filter(|(_, entry)| entry.active)
        .map(|(key, _)| key.as_str())
        .collect();
    if !running.is_empty() {
        eprintln!(
            "warning: feature '{name}' has active agents ({}); files under them will change",
            running.join(", ")
        );
    }

    if let Err(err) = git::rebase(&worktree, &target) {
        if !git::rebase_in_progress(&worktree)? {
            return Err(err);
        }
        let detail = match err {
            PmError::Git(stderr) => stderr,
            other => other.to_string(),
        };
        let rerun = match onto {
            Some(onto) => format!("pm feat rebase {name} --onto {onto}"),
            None => format!("pm feat rebase {name}"),
        };
        return Err(PmError::SafetyCheck(format!(
            "rebase of feature '{name}' onto '{target}' stopped on conflicts in {}:\n{}\n\n{detail}\n\n\
             Resolve them there, then `git rebase --continue` (or `git rebase --abort` to give up). \
             Once the rebase completes, re-run `{rerun}` to record the new base.",
            worktree.display(),
            git::status_short(&worktree)?,
        )));
    }

    state.base = target.clone();
    state.save(&features_dir, name)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feat_merge;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn commit_file(worktree: &Path, file: &str, content: &str, message: &str) {
        std::fs::write(worktree.join(file), content).unwrap();
        git::stage_file(worktree, file).unwrap();
        git::commit(worktree, message).unwrap();
    }

    fn load(project_path: &Path, name: &str) -> FeatureState {
        FeatureState::load(&paths::features_dir(project_path), name).unwrap()
    }

    #[test]
    fn rebase_orphaned_child_onto_main_records_base_and_unblocks_merge() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir) = server.setup_orphaned_child(dir.path());
        let child_wt = project_path.join("child");

        let recorded = feat_rebase(&project_path, &projects_dir, "child", Some("master")).unwrap();

        assert_eq!(recorded, "master");
        assert_eq!(load(&project_path, "child").base, "master");
        assert!(git::branch_merged_into(&child_wt, "master", "child").unwrap());
        assert!(child_wt.join("child.txt").exists());

        feat_merge::feat_merge(&project_path, &projects_dir, "child", false, server.name())
            .unwrap();
        assert!(
            paths::main_worktree(&project_path)
                .join("child.txt")
                .exists()
        );
    }

    #[test]
    fn conflicting_rebase_keeps_old_base_until_finished_with_git_and_rerun() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let main_repo = paths::main_worktree(&project_path);
        let worktree = project_path.join("login");
        commit_file(&main_repo, "shared.txt", "main content", "main change");
        git::create_branch_from(&main_repo, "other", "main").unwrap();
        commit_file(&worktree, "shared.txt", "feature content", "feature change");
        assert_eq!(load(&project_path, "login").base, "main");

        let err = feat_rebase(&project_path, &projects_dir, "login", Some("other")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("onto 'other' stopped on conflicts"), "{msg}");
        assert!(msg.contains("AA shared.txt"), "{msg}");
        assert!(
            msg.contains("re-run `pm feat rebase login --onto other`"),
            "{msg}"
        );
        assert!(git::rebase_in_progress(&worktree).unwrap());
        assert_eq!(load(&project_path, "login").base, "main");

        let err = feat_rebase(&project_path, &projects_dir, "login", Some("other")).unwrap_err();
        assert!(err.to_string().contains("rebase in progress"), "{err}");
        assert_eq!(load(&project_path, "login").base, "main");

        std::fs::write(worktree.join("shared.txt"), "resolved").unwrap();
        git::stage_file(&worktree, "shared.txt").unwrap();
        let status = std::process::Command::new("git")
            .args(["-C", &worktree.to_string_lossy(), "rebase", "--continue"])
            .env("GIT_EDITOR", "true")
            .status()
            .unwrap();
        assert!(status.success());

        let recorded = feat_rebase(&project_path, &projects_dir, "login", Some("other")).unwrap();
        assert_eq!(recorded, "other");
        assert_eq!(load(&project_path, "login").base, "other");
        assert!(!git::rebase_in_progress(&worktree).unwrap());
        assert!(git::branch_merged_into(&worktree, "other", "login").unwrap());
        assert_eq!(
            std::fs::read_to_string(worktree.join("shared.txt")).unwrap(),
            "resolved"
        );
    }

    #[test]
    fn rebase_refuses_own_branch_dirty_worktree_and_unknown_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let worktree = project_path.join("login");
        let head_before = git::run_git(&worktree, &["rev-parse", "HEAD"]).unwrap();

        let err = feat_rebase(&project_path, &projects_dir, "login", Some("login")).unwrap_err();
        assert!(err.to_string().contains("onto its own branch"), "{err}");

        let err = feat_rebase(&project_path, &projects_dir, "login", Some("nope")).unwrap_err();
        assert!(
            matches!(err, PmError::BranchNotFound(ref b) if b == "nope"),
            "{err}"
        );

        std::fs::write(worktree.join("dirty.txt"), "uncommitted").unwrap();
        git::stage_file(&worktree, "dirty.txt").unwrap();
        let err = feat_rebase(&project_path, &projects_dir, "login", None).unwrap_err();
        assert!(err.to_string().contains("uncommitted changes"), "{err}");

        let state = load(&project_path, "login");
        assert_eq!(state.base, "main");
        assert_eq!(state.branch, "login");
        assert_eq!(
            git::run_git(&worktree, &["rev-parse", "HEAD"]).unwrap(),
            head_before
        );
    }

    #[test]
    fn rebase_refuses_when_worktree_is_not_on_the_feature_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let main_repo = paths::main_worktree(&project_path);
        let worktree = project_path.join("login");
        commit_file(&main_repo, "main.txt", "main", "advance main");
        let login_before = git::run_git(&worktree, &["rev-parse", "login"]).unwrap();

        git::run_git(&worktree, &["checkout", "-b", "other"]).unwrap();
        let err = feat_rebase(&project_path, &projects_dir, "login", Some("main")).unwrap_err();
        assert!(err.to_string().contains("has 'other' checked out"), "{err}");

        git::run_git(&worktree, &["checkout", "--detach"]).unwrap();
        let err = feat_rebase(&project_path, &projects_dir, "login", Some("main")).unwrap_err();
        assert!(err.to_string().contains("detached HEAD"), "{err}");

        assert_eq!(load(&project_path, "login").base, "main");
        assert_eq!(
            git::run_git(&worktree, &["rev-parse", "login"]).unwrap(),
            login_before
        );
    }
}
