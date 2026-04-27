use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{git, tmux};

/// Rename a feature: update branch, worktree, tmux session, and state file.
pub fn feat_rename(
    project_root: &Path,
    old_name: &str,
    new_name: &str,
    tmux_server: Option<&str>,
) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    // Validate: old feature must exist
    let state = FeatureState::load(&features_dir, old_name)?;

    // Validate: new name must not already exist as a feature
    if FeatureState::exists(&features_dir, new_name) {
        return Err(PmError::FeatureAlreadyExists(new_name.to_string()));
    }

    // Validate: new name must not already exist as a branch
    let main_repo = paths::main_worktree(project_root);
    if git::branch_exists(&main_repo, new_name)? {
        return Err(PmError::SafetyCheck(format!(
            "branch '{new_name}' already exists"
        )));
    }

    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let old_worktree_path = project_root.join(&state.worktree);
    let new_worktree_path = project_root.join(new_name);

    // Step 1: Rename git branch
    git::rename_branch(&main_repo, &state.branch, new_name)?;

    // Step 2: Move git worktree
    if old_worktree_path.exists()
        && let Err(e) = git::move_worktree(&main_repo, &old_worktree_path, &new_worktree_path)
    {
        // Rollback branch rename
        let _ = git::rename_branch(&main_repo, new_name, &state.branch);
        return Err(e);
    }

    // Step 3: Rename tmux session
    let old_session = tmux::session_name(project_name, old_name);
    let new_session = tmux::session_name(project_name, new_name);
    if tmux::has_session(tmux_server, &old_session)?
        && let Err(e) = tmux::rename_session(tmux_server, &old_session, &new_session)
    {
        // Rollback worktree move and branch rename
        let _ = git::move_worktree(&main_repo, &new_worktree_path, &old_worktree_path);
        let _ = git::rename_branch(&main_repo, new_name, &state.branch);
        return Err(e);
    }

    // Step 4: Update state file (save new, delete old).
    // Capture the original branch name before the struct-update moves the
    // rest of `state` out — we still need it for the rollback path.
    let original_branch = state.branch.clone();
    let updated = FeatureState {
        branch: new_name.to_string(),
        worktree: new_name.to_string(),
        ..state
    };
    if let Err(e) = updated.save(&features_dir, new_name) {
        // Rollback everything. Use `original_branch` (the original branch
        // name) — NOT `old_name` (the feature name). They can differ for
        // adopted features whose branch name was preserved via
        // --name-override (e.g. branch="ciaran/eval", feature="eval").
        let _ = tmux::rename_session(tmux_server, &new_session, &old_session);
        let _ = git::move_worktree(&main_repo, &new_worktree_path, &old_worktree_path);
        let _ = git::rename_branch(&main_repo, new_name, &original_branch);
        return Err(e);
    }
    // Only delete old state after new one is safely written
    let _ = FeatureState::delete(&features_dir, old_name);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn rename_updates_state_file() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
        assert!(FeatureState::exists(&features_dir, "auth"));

        let state = FeatureState::load(&features_dir, "auth").unwrap();
        assert_eq!(state.branch, "auth");
        assert_eq!(state.worktree, "auth");
    }

    #[test]
    fn rename_updates_git_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", server.name()).unwrap();

        let main_repo = paths::main_worktree(&project_path);
        assert!(!git::branch_exists(&main_repo, "login").unwrap());
        assert!(git::branch_exists(&main_repo, "auth").unwrap());
    }

    #[test]
    fn rename_moves_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", server.name()).unwrap();

        assert!(!project_path.join("login").exists());
        assert!(project_path.join("auth").exists());
        assert!(project_path.join("auth").is_dir());
    }

    #[test]
    fn rename_updates_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", server.name()).unwrap();

        assert!(
            !tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "auth")).unwrap()
        );
    }

    #[test]
    fn rename_preserves_state_fields() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        let features_dir = paths::features_dir(&project_path);
        let original = FeatureState::load(&features_dir, "login").unwrap();

        feat_rename(&project_path, "login", "auth", server.name()).unwrap();

        let renamed = FeatureState::load(&features_dir, "auth").unwrap();
        assert_eq!(renamed.status, original.status);
        assert_eq!(renamed.context, original.context);
        assert_eq!(renamed.created, original.created);
        assert_eq!(renamed.pr, original.pr);
        assert_eq!(renamed.base, original.base);
    }

    #[test]
    fn rename_nonexistent_feature_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        let result = feat_rename(&project_path, "nonexistent", "new-name", server.name());
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn rename_to_existing_feature_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "signup",
            server.name(),
        ))
        .unwrap();

        let result = feat_rename(&project_path, "login", "signup", server.name());
        assert!(matches!(
            result.unwrap_err(),
            PmError::FeatureAlreadyExists(_)
        ));

        // Original feature should be untouched
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn rename_to_existing_branch_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Create a branch without a feature
        let main_repo = paths::main_worktree(&project_path);
        git::create_branch(&main_repo, "taken-branch").unwrap();

        let result = feat_rename(&project_path, "login", "taken-branch", server.name());
        assert!(result.is_err());

        // Original feature should be untouched
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(git::branch_exists(&main_repo, "login").unwrap());
    }

    // --- Rollback path tests ---

    #[test]
    fn rename_worktree_move_failure_rolls_back_branch() {
        // Branch rename succeeds, but `git worktree move` fails because the
        // destination path already exists. The rollback must rename the
        // branch back to its original name.
        //
        // Note: `git worktree move login auth` treats an existing directory
        // at `auth` as a target into which `login` should be moved (becomes
        // `auth/login`). To force a real failure we plant a regular file at
        // the destination — git refuses with "fatal: 'auth' already exists".
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        let dest = project_path.join("auth");
        std::fs::write(&dest, "blocker").unwrap();

        let result = feat_rename(&project_path, "login", "auth", server.name());
        assert!(result.is_err(), "expected worktree move to fail");

        // Branch rename rolled back: original branch exists, new doesn't.
        let main_repo = paths::main_worktree(&project_path);
        assert!(git::branch_exists(&main_repo, "login").unwrap());
        assert!(!git::branch_exists(&main_repo, "auth").unwrap());

        // Worktree should still be at its original path.
        assert!(project_path.join("login").exists());

        // State file should still exist under the old name.
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(!FeatureState::exists(&features_dir, "auth"));

        // Tmux session should still be the original — rename was never tried.
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
    }

    #[test]
    fn rename_tmux_rename_failure_rolls_back_branch_and_worktree() {
        // Branch rename + worktree move succeed, but tmux rename-session
        // fails because a session with the new name already exists. The
        // rollback must restore the worktree path AND the branch name.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        // Pre-create a tmux session with the destination name so
        // `tmux rename-session` refuses.
        tmux::create_session(
            server.name(),
            &tmux::session_name(&project_name, "auth"),
            dir.path(),
        )
        .unwrap();

        let result = feat_rename(&project_path, "login", "auth", server.name());
        assert!(result.is_err(), "expected tmux rename to fail");

        // Branch and worktree should be restored to original locations.
        let main_repo = paths::main_worktree(&project_path);
        assert!(git::branch_exists(&main_repo, "login").unwrap());
        assert!(!git::branch_exists(&main_repo, "auth").unwrap());
        assert!(project_path.join("login").exists());
        assert!(!project_path.join("auth").exists());

        // State file untouched (Step 4 never ran).
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(!FeatureState::exists(&features_dir, "auth"));

        // The original session must still be there. The colliding session
        // we pre-created also remains; its presence is what triggered the
        // failure, and rollback shouldn't touch unrelated sessions.
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "auth")).unwrap()
        );
    }

    #[test]
    fn rename_state_save_failure_rolls_back_everything() {
        // Branch + worktree + tmux all succeed, but writing the new state
        // file fails because the atomic-write tmp path is occupied by a
        // directory. The rollback must reverse all three earlier steps.
        //
        // We can't block via `<features_dir>/auth.toml` (a dir there would
        // trip the early `FeatureState::exists("auth")` validation, so the
        // function would fail before reaching the rollback path). Instead
        // we plant the blocker at `.auth.toml.tmp` — the path that
        // `FeatureState::save` writes to before the atomic rename — which
        // makes `std::fs::write` fail with EISDIR but is invisible to
        // `FeatureState::exists`.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        let features_dir = paths::features_dir(&project_path);
        let blocker = features_dir.join(".auth.toml.tmp");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("blocker"), "x").unwrap();

        let result = feat_rename(&project_path, "login", "auth", server.name());
        assert!(result.is_err(), "expected state save to fail");

        // Branch back to original name.
        let main_repo = paths::main_worktree(&project_path);
        assert!(git::branch_exists(&main_repo, "login").unwrap());
        assert!(!git::branch_exists(&main_repo, "auth").unwrap());

        // Worktree back to original path.
        assert!(project_path.join("login").exists());
        assert!(!project_path.join("auth").exists());

        // Old state file still exists; the new one was never persisted.
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(!FeatureState::exists(&features_dir, "auth"));

        // Tmux session back to original name.
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
        assert!(
            !tmux::has_session(server.name(), &tmux::session_name(&project_name, "auth")).unwrap()
        );

        // Clean up the blocker so tempdir teardown doesn't trip on the
        // unexpected directory.
        std::fs::remove_dir_all(&blocker).unwrap();
    }

    #[test]
    fn rename_state_save_failure_restores_branch_with_slash() {
        // Regression test for the "old_name vs state.branch" bug: when the
        // adopted feature's branch name differs from its feature name
        // (e.g. branch="ciaran/eval", feature="eval"), a state-save
        // failure during rename must restore the *branch name*, not the
        // feature name.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        // Create a slash-bearing branch and adopt it with --name-override
        // so feature_name ("eval") differs from branch ("ciaran/eval").
        let main_repo = paths::main_worktree(&project_path);
        git::create_branch(&main_repo, "ciaran/eval").unwrap();
        crate::commands::feat_adopt::feat_adopt(&crate::commands::feat_adopt::FeatAdoptParams {
            project_root: &project_path,
            name: "ciaran/eval",
            name_override: Some("eval"),
            context: None,
            from: None,
            edit: false,
            agent_override: None,
            tmux_server: server.name(),
            claude_base: None,
        })
        .unwrap();

        // Confirm the divergence we're testing exists on disk.
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "eval").unwrap();
        assert_eq!(state.branch, "ciaran/eval");
        assert_eq!(state.worktree, "eval");

        // Block the atomic-write tmp path so save fails (see the
        // `rename_state_save_failure_rolls_back_everything` test for why
        // we don't block `auth.toml` directly).
        let blocker = features_dir.join(".auth.toml.tmp");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("blocker"), "x").unwrap();

        let result = feat_rename(&project_path, "eval", "auth", server.name());
        assert!(result.is_err(), "expected state save to fail");

        // The original slash-bearing branch must be restored — NOT a flat
        // "eval" branch, which is what the buggy rollback would have
        // produced.
        assert!(
            git::branch_exists(&main_repo, "ciaran/eval").unwrap(),
            "rollback must restore the original branch name, slashes and all"
        );
        assert!(!git::branch_exists(&main_repo, "eval").unwrap());
        assert!(!git::branch_exists(&main_repo, "auth").unwrap());

        // Worktree back at the original feature-name path.
        assert!(project_path.join("eval").exists());
        assert!(!project_path.join("auth").exists());

        // State file still exists under the old feature name; no new one.
        assert!(FeatureState::exists(&features_dir, "eval"));
        assert!(!FeatureState::exists(&features_dir, "auth"));

        std::fs::remove_dir_all(&blocker).unwrap();
    }
}
