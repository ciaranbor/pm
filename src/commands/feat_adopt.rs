use std::path::Path;

use chrono::Utc;

use crate::error::{PmError, Result};
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{git, tmux};

/// Adopt an existing branch as a pm feature: worktree + tmux session + state file.
/// Unlike `feat_new`, this does not create a branch — it must already exist.
///
/// The tmux `server` parameter allows tests to use an isolated tmux server.
/// In production, pass `None` to use the default server.
pub fn feat_adopt(
    project_root: &Path,
    name: &str,
    context: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    // Check for duplicate
    if FeatureState::exists(&features_dir, name) {
        return Err(PmError::FeatureAlreadyExists(name.to_string()));
    }

    // Verify branch exists
    let main_worktree = project_root.join("main");
    if !git::branch_exists(&main_worktree, name)? {
        return Err(PmError::BranchNotFound(name.to_string()));
    }

    // Load project config for name
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Resolve context upfront (file contents or literal text)
    let resolved_context = context.map(super::feat_new::resolve_context).transpose()?;

    // Step 1: Write state with status = initializing
    let now = Utc::now();
    let mut state = FeatureState {
        status: FeatureStatus::Initializing,
        branch: name.to_string(),
        worktree: name.to_string(),
        base: String::new(),
        pr: String::new(),
        context: resolved_context.clone().unwrap_or_default(),
        created: now,
        last_active: now,
    };
    state.save(&features_dir, name)?;

    // Step 2: Create git worktree (skip branch creation — branch already exists)
    let worktree_path = project_root.join(name);
    git::add_worktree(&main_worktree, &worktree_path, name)?;

    // Step 2.5: Write TASK.md if context provided
    if let Some(ref resolved) = resolved_context {
        std::fs::write(worktree_path.join("TASK.md"), resolved)?;
        git::exclude_pattern(&worktree_path, "TASK.md")?;
    }

    // Step 3: Create tmux session
    let session_name = format!("{project_name}/{name}");
    tmux::create_session(tmux_server, &session_name, &worktree_path)?;

    // Step 3.5: If context provided, open a claude session in a new window to read TASK.md
    if resolved_context.is_some() {
        let window_target =
            tmux::new_window(tmux_server, &session_name, &worktree_path, Some("claude"))?;
        tmux::send_keys(tmux_server, &window_target, "claude 'READ TASK.md'")?;
    }

    // Step 4: Update status to wip
    state.status = FeatureStatus::Wip;
    state.last_active = Utc::now();
    state.save(&features_dir, name)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn setup_project(dir: &Path, server: &TestServer) -> std::path::PathBuf {
        let project_path = dir.join("myapp");
        let projects_dir = dir.join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        project_path
    }

    /// Create a branch on the main worktree so feat_adopt can find it.
    fn create_branch(project_path: &Path, name: &str) {
        let main_worktree = project_path.join("main");
        git::create_branch(&main_worktree, name).unwrap();
    }

    #[test]
    fn feat_adopt_creates_state_file_with_wip_status() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        create_branch(&project_path, "login");

        feat_adopt(&project_path, "login", None, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);
    }

    #[test]
    fn feat_adopt_creates_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        create_branch(&project_path, "login");

        feat_adopt(&project_path, "login", None, server.name()).unwrap();

        let worktree_path = project_path.join("login");
        assert!(worktree_path.exists());
        assert!(worktree_path.is_dir());
    }

    #[test]
    fn feat_adopt_creates_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        create_branch(&project_path, "login");

        feat_adopt(&project_path, "login", None, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), "myapp/login").unwrap());
    }

    #[test]
    fn feat_adopt_does_not_create_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        create_branch(&project_path, "login");

        // Branch exists before adopt
        let main_wt = project_path.join("main");
        assert!(git::branch_exists(&main_wt, "login").unwrap());

        feat_adopt(&project_path, "login", None, server.name()).unwrap();

        // Branch still exists (not a new one, same one)
        assert!(git::branch_exists(&main_wt, "login").unwrap());
    }

    #[test]
    fn feat_adopt_fails_when_branch_does_not_exist() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);

        let result = feat_adopt(&project_path, "nonexistent", None, server.name());

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::BranchNotFound(_)));
    }

    #[test]
    fn feat_adopt_fails_when_feature_already_exists() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        create_branch(&project_path, "login");

        feat_adopt(&project_path, "login", None, server.name()).unwrap();
        let result = feat_adopt(&project_path, "login", None, server.name());

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PmError::FeatureAlreadyExists(_)
        ));
    }

    #[test]
    fn feat_adopt_with_context_writes_task_md() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        create_branch(&project_path, "login");

        feat_adopt(
            &project_path,
            "login",
            Some("Adopt existing login branch"),
            server.name(),
        )
        .unwrap();

        let task_md = project_path.join("login").join("TASK.md");
        assert!(task_md.exists());
        let content = std::fs::read_to_string(&task_md).unwrap();
        assert_eq!(content, "Adopt existing login branch");
    }

    #[test]
    fn feat_adopt_sets_timestamps() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        create_branch(&project_path, "login");
        let before = Utc::now();

        feat_adopt(&project_path, "login", None, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert!(state.created >= before);
        assert!(state.last_active >= state.created);
    }

    #[test]
    fn feat_adopt_with_context_creates_claude_window() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        create_branch(&project_path, "login");

        feat_adopt(
            &project_path,
            "login",
            Some("Adopt existing login branch"),
            server.name(),
        )
        .unwrap();

        // Session should have 2 windows: the default shell + the claude window
        let output = tmux::list_windows(server.name(), "myapp/login").unwrap();
        assert_eq!(output, 2);
    }

    #[test]
    fn feat_adopt_without_context_has_single_window() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        create_branch(&project_path, "login");

        feat_adopt(&project_path, "login", None, server.name()).unwrap();

        let output = tmux::list_windows(server.name(), "myapp/login").unwrap();
        assert_eq!(output, 1);
    }

    #[test]
    fn feat_adopt_tmux_failure_leaves_initializing_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        create_branch(&project_path, "login");

        // Pre-create a tmux session to cause a conflict
        tmux::create_session(server.name(), "myapp/login", dir.path()).unwrap();

        let result = feat_adopt(&project_path, "login", None, server.name());
        assert!(result.is_err());

        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Initializing);
    }
}
