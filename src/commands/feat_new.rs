use std::path::Path;

use chrono::Utc;

use crate::commands::claude_settings;
use crate::commands::feat_common::{self, InitStateFields};
use crate::error::{PmError, Result};
use crate::hooks;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{git, tmux};

/// Derive a feature name from a branch name, replacing `/` with `-`.
/// If `name_override` is provided, validate it and use that instead.
pub fn sanitize_feature_name(branch: &str, name_override: Option<&str>) -> Result<String> {
    match name_override {
        Some(name) => {
            if name.contains('/') {
                return Err(PmError::InvalidFeatureName(name.to_string()));
            }
            Ok(name.to_string())
        }
        None => Ok(branch.replace('/', "-")),
    }
}

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

/// Resolve the base branch for a new feature.
/// If explicitly provided, use that. Otherwise detect the current branch from `cwd`.
pub fn resolve_base(project_root: &Path, base: Option<&str>, cwd: &Path) -> Result<String> {
    if let Some(b) = base {
        return Ok(b.to_string());
    }
    // Detect from CWD: find which worktree we're in and get its branch
    let main_worktree = project_root.join("main");
    // Try CWD first, fall back to main worktree
    let detect_from = if cwd.starts_with(project_root) {
        cwd
    } else {
        main_worktree.as_path()
    };
    git::current_branch(detect_from).or_else(|_| Ok("main".to_string()))
}

/// Create a new feature: branch + worktree + tmux session + state file.
///
/// The `base` parameter specifies which branch to stack on. When `None`,
/// the current branch is detected from CWD (enabling natural stacking from
/// within a feature worktree).
///
/// The tmux `server` parameter allows tests to use an isolated tmux server.
/// In production, pass `None` to use the default server.
#[allow(clippy::too_many_arguments)]
pub fn feat_new(
    project_root: &Path,
    name: &str,
    name_override: Option<&str>,
    context: Option<&str>,
    base: Option<&str>,
    no_edit: bool,
    agent_override: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<String> {
    let branch = name;
    let feature_name = sanitize_feature_name(branch, name_override)?;
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    // Check for duplicate
    if FeatureState::exists(&features_dir, &feature_name) {
        return Err(PmError::FeatureAlreadyExists(feature_name));
    }

    // Load project config for name
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Resolve context upfront (file contents or literal text)
    let resolved_context = context.map(resolve_context).transpose()?;

    // Resolve base branch (explicit, or detected from CWD)
    let cwd = std::env::current_dir()?;
    let resolved_base = resolve_base(project_root, base, &cwd)?;

    // Step 1: Write state with status = initializing
    let mut state = feat_common::write_initializing_state(
        &features_dir,
        &feature_name,
        InitStateFields {
            branch,
            worktree: &feature_name,
            base: &resolved_base,
            pr: "",
            context: resolved_context.as_deref().unwrap_or(""),
        },
    )?;

    // Steps 2-5: Create resources, rolling back on failure
    let main_worktree = project_root.join("main");
    let worktree_path = project_root.join(&feature_name);
    let session_name = format!("{project_name}/{feature_name}");
    let hook_path = project_root.join(hooks::POST_CREATE_PATH);

    let result: Result<()> = (|| {
        // Step 2: Create git branch from the base branch (uses actual branch name, may contain slashes)
        git::create_branch_from(&main_worktree, branch, &resolved_base)?;

        // Step 3: Create git worktree
        git::add_worktree(&main_worktree, &worktree_path, branch)?;

        // Step 3.5: Seed Claude Code settings from main worktree
        claude_settings::seed_feature_claude(project_root, &worktree_path)?;

        // Step 3.6: Enqueue initial context as a message to the default agent
        // (if context provided). The Stop hook will deliver it on the empty
        // first turn after spawn. TASK.md is never written.
        if let Some(ref resolved) = resolved_context {
            feat_common::enqueue_initial_context(
                project_root,
                &feature_name,
                &config,
                agent_override,
                resolved,
            )?;
        }

        // Step 4: Create tmux session
        tmux::create_session(tmux_server, &session_name, &worktree_path)?;

        // Step 4.5: Spawn the default claude agent (if context was provided).
        // The agent starts with no positional prompt; the Stop hook trampoline
        // drives it into `pm msg wait` on its first stop.
        if resolved_context.is_some() {
            feat_common::spawn_default_agent(
                project_root,
                &feature_name,
                &config,
                agent_override,
                no_edit,
                tmux_server,
            )?;
        }

        // Step 4.6: Run post-create hook in a named "hook" window (non-fatal)
        hooks::run_hook(tmux_server, &session_name, &worktree_path, &hook_path);

        // Step 5: Update status to wip
        state.status = FeatureStatus::Wip;
        state.last_active = Utc::now();
        state.save(&features_dir, &feature_name)?;

        Ok(())
    })();

    if let Err(e) = result {
        feat_common::rollback_creation(
            project_root,
            &feature_name,
            branch,
            project_name,
            tmux_server,
            true, // feat_new owns the branch and may destroy it on failure
        );
        return Err(e);
    }

    Ok(feature_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn feat_new_creates_all_resources() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        let before = Utc::now();

        feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        // State file with correct status and fields
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);
        assert_eq!(state.branch, "login");
        assert_eq!(state.worktree, "login");
        assert!(state.created >= before);
        assert!(state.last_active >= state.created);

        // Git branch exists
        let main_path = project_path.join("main");
        assert!(git::branch_exists(&main_path, "login").unwrap());

        // Worktree directory exists
        let worktree_path = project_path.join("login");
        assert!(worktree_path.exists());
        assert!(worktree_path.is_dir());

        // Tmux session exists
        assert!(tmux::has_session(server.name(), &format!("{project_name}/login")).unwrap());
    }

    #[test]
    fn feat_new_duplicate_name_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();
        let result = feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        );

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PmError::FeatureAlreadyExists(_)
        ));
    }

    #[test]
    fn feat_new_tmux_failure_cleans_up_all_resources() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());

        // Pre-create a tmux session with the name feat_new will use,
        // so create_session fails with "duplicate session"
        tmux::create_session(server.name(), &format!("{project_name}/login"), dir.path()).unwrap();

        let result = feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        );
        assert!(result.is_err());

        // State file should be cleaned up
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));

        // Branch and worktree should be cleaned up
        let main_path = project_path.join("main");
        assert!(!git::branch_exists(&main_path, "login").unwrap());
        assert!(!project_path.join("login").exists());
    }

    #[test]
    fn feat_new_worktree_failure_cleans_up_branch_and_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        // Pre-create the worktree path so add_worktree fails
        std::fs::create_dir(project_path.join("login")).unwrap();
        std::fs::write(project_path.join("login").join("blocker.txt"), "").unwrap();

        let result = feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        );
        assert!(result.is_err());

        // State file should be cleaned up
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));

        // Branch should be cleaned up (worktree was never created by git)
        let main_path = project_path.join("main");
        assert!(!git::branch_exists(&main_path, "login").unwrap());
    }

    #[test]
    fn feat_new_with_text_context_enqueues_message() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        feat_new(
            &project_path,
            "login",
            None,
            Some("Implement login page per issue #42"),
            None,
            false,
            Some("implementer"),
            server.name(),
        )
        .unwrap();

        // No TASK.md on disk any more.
        assert!(!project_path.join("login").join("TASK.md").exists());

        // The context is queued as a message in the resolved agent's inbox.
        let messages_dir = paths::messages_dir(&project_path);
        let summaries = crate::messages::list(&messages_dir, "login", "implementer", None).unwrap();
        assert_eq!(summaries.len(), 1);
        let msg = crate::messages::read_at(
            &messages_dir,
            "login",
            "implementer",
            &summaries[0].sender,
            summaries[0].index,
        )
        .unwrap()
        .unwrap();
        assert!(msg.body.contains("Implement login page per issue #42"));
    }

    #[test]
    fn feat_new_with_file_context_reads_file() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        // Create a temp file with context content
        let brief_path = dir.path().join("brief.md");
        std::fs::write(&brief_path, "# Login Feature\nBuild the login page").unwrap();

        feat_new(
            &project_path,
            "login",
            None,
            Some(brief_path.to_str().unwrap()),
            None,
            false,
            Some("implementer"),
            server.name(),
        )
        .unwrap();

        // No TASK.md; content is queued to the resolved agent's inbox.
        assert!(!project_path.join("login").join("TASK.md").exists());
        let messages_dir = paths::messages_dir(&project_path);
        let summaries = crate::messages::list(&messages_dir, "login", "implementer", None).unwrap();
        assert_eq!(summaries.len(), 1);
        let msg = crate::messages::read_at(
            &messages_dir,
            "login",
            "implementer",
            &summaries[0].sender,
            summaries[0].index,
        )
        .unwrap()
        .unwrap();
        assert!(msg.body.contains("# Login Feature\nBuild the login page"));
    }

    #[test]
    fn feat_new_with_context_stores_resolved_content_in_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        // Pass a file path as context — state should store the file contents, not the path
        let brief_path = dir.path().join("brief.md");
        std::fs::write(&brief_path, "resolved file content").unwrap();

        feat_new(
            &project_path,
            "login",
            None,
            Some(brief_path.to_str().unwrap()),
            None,
            false,
            None,
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
        let (project_path, _, project_name) = server.setup_project(dir.path());

        feat_new(
            &project_path,
            "login",
            None,
            Some("Build the login page"),
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        // Session should have 3 windows: the default shell + the claude window + hook window
        let output = tmux::list_windows(server.name(), &format!("{project_name}/login")).unwrap();
        assert_eq!(output, 3);
    }

    #[test]
    fn feat_new_with_agent_override_spawns_named_agent() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());

        feat_new(
            &project_path,
            "login",
            None,
            Some("Build the login page"),
            None,
            false,
            Some("researcher"),
            server.name(),
        )
        .unwrap();

        // The agent window should be named "researcher" (not "claude")
        let session = format!("{project_name}/login");
        let target = tmux::find_window(server.name(), &session, "researcher").unwrap();
        assert!(target.is_some(), "expected a 'researcher' tmux window");

        // The agent should be registered in the agent registry
        let agents_dir = paths::agents_dir(&project_path);
        let registry = crate::state::agent::AgentRegistry::load(&agents_dir, "login").unwrap();
        let entry = registry.get("researcher");
        assert!(entry.is_some(), "expected 'researcher' in agent registry");
        assert!(entry.unwrap().active);
    }

    #[test]
    fn feat_new_without_context_has_shell_and_hook_windows() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());

        feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        // 2 windows: default shell + hook window
        let session = format!("{project_name}/login");
        let windows = tmux::list_windows(server.name(), &session).unwrap();
        assert_eq!(windows, 2);
        // Hook window should be named "hook"
        let target = tmux::find_window(server.name(), &session, "hook").unwrap();
        assert!(target.is_some());

        // No context → no TASK.md (ever) and no queued messages.
        let task_md = project_path.join("login").join("TASK.md");
        assert!(!task_md.exists());
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.context, "");
    }

    #[test]
    fn feat_new_skips_hook_when_script_removed() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());

        // Remove the bootstrapped hook script
        std::fs::remove_file(project_path.join(hooks::POST_CREATE_PATH)).unwrap();

        feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        // Only 1 window — hook was skipped because file is missing
        let windows = tmux::list_windows(server.name(), &format!("{project_name}/login")).unwrap();
        assert_eq!(windows, 1);
    }

    #[test]
    fn feat_new_with_base_stores_base_in_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();
        feat_new(
            &project_path,
            "stacked",
            None,
            None,
            Some("login"),
            false,
            None,
            server.name(),
        )
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "stacked").unwrap();
        assert_eq!(state.base, "login");
    }

    #[test]
    fn feat_new_with_base_branches_from_parent() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        // Create parent feature with a commit
        feat_new(
            &project_path,
            "parent",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();
        let parent_wt = project_path.join("parent");
        std::fs::write(parent_wt.join("parent.txt"), "parent work").unwrap();
        git::stage_file(&parent_wt, "parent.txt").unwrap();
        git::commit(&parent_wt, "parent commit").unwrap();

        // Create stacked feature based on parent
        feat_new(
            &project_path,
            "child",
            None,
            None,
            Some("parent"),
            false,
            None,
            server.name(),
        )
        .unwrap();

        // Child worktree should have the parent's file
        let child_wt = project_path.join("child");
        assert!(child_wt.join("parent.txt").exists());
    }

    #[test]
    fn feat_new_without_base_defaults_to_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.base, "main");
    }

    #[test]
    fn resolve_base_returns_explicit_base() {
        let dir = tempdir().unwrap();
        let result = resolve_base(dir.path(), Some("my-branch"), dir.path()).unwrap();
        assert_eq!(result, "my-branch");
    }

    #[test]
    fn resolve_base_detects_branch_from_worktree_cwd() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myproject");
        std::fs::create_dir_all(&project_path).unwrap();
        let main_path = project_path.join("main");
        git::init_repo(&main_path).unwrap();

        git::create_branch(&main_path, "parent").unwrap();
        let parent_wt = project_path.join("parent");
        git::add_worktree(&main_path, &parent_wt, "parent").unwrap();

        // Simulate CWD being inside the parent worktree
        let result = resolve_base(&project_path, None, &parent_wt).unwrap();
        assert_eq!(result, "parent");
    }

    #[test]
    fn resolve_base_falls_back_to_main_when_outside_project() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myproject");
        std::fs::create_dir_all(&project_path).unwrap();
        let main_path = project_path.join("main");
        git::init_repo(&main_path).unwrap();

        // CWD is outside the project
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let result = resolve_base(&project_path, None, &outside).unwrap();
        assert_eq!(result, "main");
    }

    #[test]
    fn sanitize_replaces_slashes_with_dashes() {
        assert_eq!(
            sanitize_feature_name("ciaran/eval", None).unwrap(),
            "ciaran-eval"
        );
        assert_eq!(
            sanitize_feature_name("feat/deep/nested", None).unwrap(),
            "feat-deep-nested"
        );
        assert_eq!(sanitize_feature_name("simple", None).unwrap(), "simple");
    }

    #[test]
    fn sanitize_uses_override_when_provided() {
        assert_eq!(
            sanitize_feature_name("ciaran/eval", Some("eval")).unwrap(),
            "eval"
        );
    }

    #[test]
    fn sanitize_rejects_override_with_slash() {
        let result = sanitize_feature_name("ciaran/eval", Some("foo/bar"));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PmError::InvalidFeatureName(_)
        ));
    }

    #[test]
    fn feat_new_slash_collision_detected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        // Create "ciaran-login" first
        feat_new(
            &project_path,
            "ciaran-login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        // "ciaran/login" sanitizes to "ciaran-login" — should conflict
        let result = feat_new(
            &project_path,
            "ciaran/login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PmError::FeatureAlreadyExists(_)
        ));
    }

    #[test]
    fn feat_new_slash_branch_sanitizes_feature_name() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());

        feat_new(
            &project_path,
            "ciaran/login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        // Feature name should be sanitized
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "ciaran-login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);
        assert_eq!(state.branch, "ciaran/login");
        assert_eq!(state.worktree, "ciaran-login");

        // Worktree dir uses sanitized name
        assert!(project_path.join("ciaran-login").exists());

        // Tmux session uses sanitized name
        assert!(tmux::has_session(server.name(), &format!("{project_name}/ciaran-login")).unwrap());
    }

    #[test]
    fn feat_new_with_name_override() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());

        feat_new(
            &project_path,
            "ciaran/eval",
            Some("eval"),
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "eval").unwrap();
        assert_eq!(state.branch, "ciaran/eval");
        assert_eq!(state.worktree, "eval");
        assert!(project_path.join("eval").exists());
        assert!(tmux::has_session(server.name(), &format!("{project_name}/eval")).unwrap());
    }
}
