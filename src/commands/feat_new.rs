use std::path::Path;

use chrono::Utc;

use crate::commands::permissions;
use crate::error::{PmError, Result};
use crate::hooks;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{git, tmux};

/// Resolve context: if the value is a path to an existing file, read its contents;
/// otherwise treat it as literal text.
pub fn resolve_context(context: &str) -> Result<String> {
    let path = Path::new(context);
    if path.is_file() {
        Ok(std::fs::read_to_string(path)?)
    } else {
        Ok(context.to_string())
    }
}

/// Create a new feature: branch + worktree + tmux session + state file.
///
/// The tmux `server` parameter allows tests to use an isolated tmux server.
/// In production, pass `None` to use the default server.
pub fn feat_new(
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

    // Load project config for name
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Resolve context upfront (file contents or literal text)
    let resolved_context = context.map(resolve_context).transpose()?;

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

    // Step 2: Create git branch
    let main_worktree = project_root.join("main");
    git::create_branch(&main_worktree, name)?;

    // Step 3: Create git worktree
    let worktree_path = project_root.join(name);
    git::add_worktree(&main_worktree, &worktree_path, name)?;

    // Step 3.5: Seed Claude Code permissions from main worktree
    permissions::seed_feature_permissions(project_root, &worktree_path)?;

    // Step 3.6: Write TASK.md if context provided
    if let Some(ref resolved) = resolved_context {
        std::fs::write(worktree_path.join("TASK.md"), resolved)?;
        git::exclude_pattern(&worktree_path, "TASK.md")?;
    }

    // Step 4: Create tmux session
    let session_name = format!("{project_name}/{name}");
    tmux::create_session(tmux_server, &session_name, &worktree_path)?;

    // Step 4.5: If context provided, open a claude session in a new window to read TASK.md
    if resolved_context.is_some() {
        let window_target = tmux::new_window(tmux_server, &session_name, &worktree_path)?;
        tmux::send_keys(tmux_server, &window_target, "claude 'READ TASK.md'")?;
    }

    // Step 4.6: Run post-create hook in a named "hook" window (non-fatal)
    let hook_path = project_root.join(hooks::POST_CREATE_PATH);
    hooks::run_hook(tmux_server, &session_name, &worktree_path, &hook_path);

    // Step 5: Update status to wip
    state.status = FeatureStatus::Wip;
    state.last_active = Utc::now();
    state.save(&features_dir, name)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use crate::hooks;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn setup_project(dir: &Path, server: &TestServer) -> (std::path::PathBuf, std::path::PathBuf) {
        let project_path = dir.join("myapp");
        let projects_dir = dir.join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        (project_path, projects_dir)
    }

    #[test]
    fn feat_new_creates_state_file_with_wip_status() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(&project_path, "login", None, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);
    }

    #[test]
    fn feat_new_creates_git_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(&project_path, "login", None, server.name()).unwrap();

        let main_path = project_path.join("main");
        assert!(git::branch_exists(&main_path, "login").unwrap());
    }

    #[test]
    fn feat_new_creates_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(&project_path, "login", None, server.name()).unwrap();

        let worktree_path = project_path.join("login");
        assert!(worktree_path.exists());
        assert!(worktree_path.is_dir());
    }

    #[test]
    fn feat_new_creates_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(&project_path, "login", None, server.name()).unwrap();

        assert!(tmux::has_session(server.name(), "myapp/login").unwrap());
    }

    #[test]
    fn feat_new_sets_timestamps() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);
        let before = Utc::now();

        feat_new(&project_path, "login", None, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert!(state.created >= before);
        assert!(state.last_active >= state.created);
    }

    #[test]
    fn feat_new_state_has_matching_branch_and_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(&project_path, "login", None, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.branch, "login");
        assert_eq!(state.worktree, "login");
    }

    #[test]
    fn feat_new_duplicate_name_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(&project_path, "login", None, server.name()).unwrap();
        let result = feat_new(&project_path, "login", None, server.name());

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PmError::FeatureAlreadyExists(_)
        ));
    }

    #[test]
    fn feat_new_tmux_failure_leaves_initializing_state_with_orphan_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        // Pre-create a tmux session with the name feat_new will use,
        // so create_session fails with "duplicate session"
        tmux::create_session(server.name(), "myapp/login", dir.path()).unwrap();

        let result = feat_new(&project_path, "login", None, server.name());
        assert!(result.is_err());

        // State file should exist with initializing status
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Initializing);

        // Branch and worktree were created before tmux failed — orphaned
        let main_path = project_path.join("main");
        assert!(git::branch_exists(&main_path, "login").unwrap());
        assert!(project_path.join("login").exists());
    }

    #[test]
    fn feat_new_worktree_path_conflict_leaves_initializing_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        // Pre-create the worktree path so add_worktree fails
        std::fs::create_dir(project_path.join("login")).unwrap();
        std::fs::write(project_path.join("login").join("blocker.txt"), "").unwrap();

        let result = feat_new(&project_path, "login", None, server.name());
        assert!(result.is_err());

        // State file should exist with initializing status
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Initializing);
    }

    #[test]
    fn feat_new_with_text_context_writes_task_md() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(
            &project_path,
            "login",
            Some("Implement login page per issue #42"),
            server.name(),
        )
        .unwrap();

        let worktree_path = project_path.join("login");
        let task_md = worktree_path.join("TASK.md");
        assert!(task_md.exists());
        let content = std::fs::read_to_string(&task_md).unwrap();
        assert_eq!(content, "Implement login page per issue #42");

        // TASK.md should be excluded from untracked files
        let untracked = git::untracked_files(&worktree_path).unwrap();
        assert!(!untracked.contains(&"TASK.md".to_string()));
    }

    #[test]
    fn feat_new_with_file_context_reads_file() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        // Create a temp file with context content
        let brief_path = dir.path().join("brief.md");
        std::fs::write(&brief_path, "# Login Feature\nBuild the login page").unwrap();

        feat_new(
            &project_path,
            "login",
            Some(brief_path.to_str().unwrap()),
            server.name(),
        )
        .unwrap();

        let task_md = project_path.join("login").join("TASK.md");
        assert!(task_md.exists());
        let content = std::fs::read_to_string(&task_md).unwrap();
        assert_eq!(content, "# Login Feature\nBuild the login page");
    }

    #[test]
    fn feat_new_with_context_stores_resolved_content_in_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        // Pass a file path as context — state should store the file contents, not the path
        let brief_path = dir.path().join("brief.md");
        std::fs::write(&brief_path, "resolved file content").unwrap();

        feat_new(
            &project_path,
            "login",
            Some(brief_path.to_str().unwrap()),
            server.name(),
        )
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.context, "resolved file content");
    }

    #[test]
    fn feat_new_with_context_creates_claude_window() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(
            &project_path,
            "login",
            Some("Build the login page"),
            server.name(),
        )
        .unwrap();

        // Session should have 3 windows: the default shell + the claude window + hook window
        let output = tmux::list_windows(server.name(), "myapp/login").unwrap();
        assert_eq!(output, 3);
    }

    #[test]
    fn feat_new_without_context_has_shell_and_hook_windows() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(&project_path, "login", None, server.name()).unwrap();

        // Session should have 2 windows: default shell + hook window
        let output = tmux::list_windows(server.name(), "myapp/login").unwrap();
        assert_eq!(output, 2);
    }

    #[test]
    fn feat_new_without_context_has_no_task_md() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(&project_path, "login", None, server.name()).unwrap();

        let task_md = project_path.join("login").join("TASK.md");
        assert!(!task_md.exists());

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.context, "");
    }

    #[test]
    fn feat_new_runs_default_post_create_hook() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(&project_path, "login", None, server.name()).unwrap();

        // Session should have 2 windows: default shell + hook window
        let windows = tmux::list_windows(server.name(), "myapp/login").unwrap();
        assert_eq!(windows, 2);
        // Hook window should be named "hook"
        let target = tmux::find_window(server.name(), "myapp/login", "hook").unwrap();
        assert!(target.is_some());
    }

    #[test]
    fn feat_new_with_context_and_hook_creates_three_windows() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        feat_new(
            &project_path,
            "login",
            Some("Build the login page"),
            server.name(),
        )
        .unwrap();

        // 3 windows: default shell + claude window + hook window
        let windows = tmux::list_windows(server.name(), "myapp/login").unwrap();
        assert_eq!(windows, 3);
    }

    #[test]
    fn feat_new_skips_hook_when_script_removed() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = setup_project(dir.path(), &server);

        // Remove the bootstrapped hook script
        std::fs::remove_file(project_path.join(hooks::POST_CREATE_PATH)).unwrap();

        feat_new(&project_path, "login", None, server.name()).unwrap();

        // Only 1 window — hook was skipped because file is missing
        let windows = tmux::list_windows(server.name(), "myapp/login").unwrap();
        assert_eq!(windows, 1);
    }
}
