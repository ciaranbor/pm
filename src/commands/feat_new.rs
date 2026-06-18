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

/// Read all of `reader` to EOF as a UTF-8 string. Used to back the `-`
/// (stdin) sentinel; factored out so tests can inject a reader.
fn read_to_string(mut reader: impl std::io::Read) -> Result<String> {
    let mut buf = String::new();
    reader.read_to_string(&mut buf)?;
    Ok(buf)
}

/// Resolve context: `-` reads the entire body from stdin; otherwise, if the
/// value is a path to an existing file, read its contents; otherwise treat it
/// as literal text.
pub fn resolve_context(context: &str) -> Result<String> {
    resolve_context_from(context, std::io::stdin())
}

fn resolve_context_from(context: &str, stdin: impl std::io::Read) -> Result<String> {
    if context == "-" {
        return read_to_string(stdin);
    }
    let path = Path::new(context);
    if path.is_file() {
        Ok(std::fs::read_to_string(path)?)
    } else {
        Ok(context.to_string())
    }
}

/// Resolve the `-` stdin sentinel only, leaving any other value untouched as a
/// literal string. Used by callers (e.g. `pm agent spawn`) that treat context
/// as a literal and must not perform file-path resolution. Only the `-` case
/// reads stdin; every other value (including `None`) is returned as-is.
pub fn resolve_stdin_context(context: Option<&str>) -> Result<Option<String>> {
    resolve_stdin_context_from(context, std::io::stdin())
}

fn resolve_stdin_context_from(
    context: Option<&str>,
    stdin: impl std::io::Read,
) -> Result<Option<String>> {
    match context {
        Some("-") => Ok(Some(read_to_string(stdin)?)),
        other => Ok(other.map(str::to_string)),
    }
}

/// Resolve the base branch for a new feature.
/// If explicitly provided, use that. Otherwise detect the current branch from `cwd`.
pub fn resolve_base(project_root: &Path, base: Option<&str>, cwd: &Path) -> Result<String> {
    if let Some(b) = base {
        return Ok(b.to_string());
    }
    // Detect from CWD: find which worktree we're in and get its branch
    let main_worktree = paths::main_worktree(project_root);
    // Try CWD first, fall back to main worktree
    let detect_from = if cwd.starts_with(project_root) {
        cwd
    } else {
        main_worktree.as_path()
    };
    git::current_branch(detect_from).or_else(|_| Ok("main".to_string()))
}

/// Parameters for creating a new feature.
pub struct FeatNewParams<'a> {
    pub project_root: &'a Path,
    pub name: &'a str,
    pub name_override: Option<&'a str>,
    pub context: Option<&'a str>,
    /// Which branch to stack on. When `None`, the current branch is detected
    /// from CWD (enabling natural stacking from within a feature worktree).
    pub base: Option<&'a str>,
    pub edit: bool,
    /// Workflow to activate for this feature. Required when `context` is
    /// provided (a context with no workflow has nobody to deliver it to).
    pub workflow: Option<&'a str>,
    /// Allows tests to use an isolated tmux server. Pass `None` in production.
    pub tmux_server: Option<&'a str>,
}

#[cfg(test)]
impl<'a> FeatNewParams<'a> {
    /// Test helper: build params with all optional fields set to defaults.
    pub fn with_defaults(
        project_root: &'a Path,
        name: &'a str,
        tmux_server: Option<&'a str>,
    ) -> Self {
        Self {
            project_root,
            name,
            name_override: None,
            context: None,
            base: None,
            edit: false,
            workflow: None,
            tmux_server,
        }
    }
}

/// Create a new feature: branch + worktree + tmux session + state file.
pub fn feat_new(params: &FeatNewParams<'_>) -> Result<String> {
    // A context with no workflow has nobody to deliver it to — fail
    // early before any side effects.
    if params.context.is_some() && params.workflow.is_none() {
        return Err(PmError::SafetyCheck(
            "--context requires --workflow <name>. \
             Run `pm workflow list` to see installed workflows."
                .to_string(),
        ));
    }

    // Check feature limit before doing any work
    crate::state::project::check_feature_limit(params.project_root)?;

    let branch = params.name;
    let feature_name = sanitize_feature_name(branch, params.name_override)?;
    let features_dir = paths::features_dir(params.project_root);
    let pm_dir = paths::pm_dir(params.project_root);

    // Check for duplicate
    if FeatureState::exists(&features_dir, &feature_name) {
        return Err(PmError::FeatureAlreadyExists(feature_name));
    }

    // Load project config for name
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Load the workflow definition (if any) and validate its auto-spawn
    // agents up-front. Fail early — before any tmux/git/state side
    // effects — so a bad workflow never leaves a half-built feature on
    // disk.
    let workflow_def = params
        .workflow
        .map(|w| feat_common::load_and_validate_workflow(params.project_root, w))
        .transpose()?;
    let auto_spawn: &[String] = workflow_def
        .as_ref()
        .map(|d| d.auto_spawn.as_slice())
        .unwrap_or(&[]);

    // If the user supplied --context, somebody has to receive it.
    // A workflow with empty `auto_spawn` would silently swallow the
    // context — block that case so the user gets a clear error.
    if params.context.is_some() && auto_spawn.is_empty() {
        return Err(PmError::SafetyCheck(format!(
            "workflow '{}' has an empty `auto_spawn` list, so --context has no recipient. \
             Add at least one agent to `auto_spawn` in the workflow's config.toml, \
             or drop --context.",
            params.workflow.unwrap_or("<none>"),
        )));
    }

    // Resolve context upfront (file contents or literal text)
    let resolved_context = params.context.map(resolve_context).transpose()?;

    // Resolve base branch (explicit, or detected from CWD)
    let cwd = std::env::current_dir()?;
    let resolved_base = resolve_base(params.project_root, params.base, &cwd)?;

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
            workflow: params.workflow,
        },
    )?;

    // Steps 2-5: Create resources, rolling back on failure
    let main_worktree = paths::main_worktree(params.project_root);
    let worktree_path = params.project_root.join(&feature_name);
    let session_name = tmux::session_name(project_name, &feature_name);
    let hook_path = params.project_root.join(hooks::POST_CREATE_PATH);

    let result: Result<()> = (|| {
        // Step 2: Create git branch from the base branch (uses actual branch name, may contain slashes)
        git::create_branch_from(&main_worktree, branch, &resolved_base)?;

        // Step 3: Create git worktree
        git::add_worktree(&main_worktree, &worktree_path, branch)?;

        // Step 3.5: Seed Claude Code settings from main worktree
        claude_settings::seed_feature_claude(params.project_root, &worktree_path)?;

        // Step 3.6: Enqueue initial context as a message to every
        // auto_spawn agent in the workflow (if context provided). The
        // Stop hook will deliver it on the empty first turn after spawn.
        // TASK.md is never written.
        if let Some(ref resolved) = resolved_context {
            feat_common::enqueue_initial_context(
                params.project_root,
                &feature_name,
                auto_spawn,
                resolved,
            )?;
        }

        // Step 4: Create tmux session
        tmux::create_session(params.tmux_server, &session_name, &worktree_path)?;

        // Step 4.5: Spawn the workflow's auto_spawn agents (if any). The
        // first agent reuses window :0 so we don't leave an empty default
        // shell behind. Subsequent agents go into fresh windows.
        if resolved_context.is_some() && !auto_spawn.is_empty() {
            let reuse_target = format!("{session_name}:0");
            feat_common::spawn_auto_spawn_agents(
                params.project_root,
                &feature_name,
                auto_spawn,
                params.edit,
                Some(&reuse_target),
                params.tmux_server,
            )?;
        }

        // Step 4.6: Run post-create hook in a named "hook" window (non-fatal)
        hooks::run_hook(
            params.tmux_server,
            &session_name,
            &worktree_path,
            &hook_path,
        );

        // Step 5: Update status to wip
        state.status = FeatureStatus::Wip;
        state.last_active = Utc::now();
        state.save(&features_dir, &feature_name)?;

        Ok(())
    })();

    if let Err(e) = result {
        feat_common::rollback_creation(&feat_common::RollbackParams {
            project_root: params.project_root,
            feature_name: &feature_name,
            branch,
            project_name,
            tmux_server: params.tmux_server,
            delete_branch: true, // feat_new owns the branch and may destroy it on failure
            base: &resolved_base,
        });
        return Err(e);
    }

    Ok(feature_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks;
    use crate::testing::TestServer;
    use std::io::Cursor;
    use tempfile::tempdir;

    #[test]
    fn resolve_context_dash_reads_stdin() {
        let body = "line one\nline two\n";
        let resolved = resolve_context_from("-", Cursor::new(body)).unwrap();
        assert_eq!(resolved, body);
    }

    #[test]
    fn resolve_context_literal_does_not_touch_stdin() {
        // Passing a non-empty stdin proves the literal path never reads it.
        let resolved = resolve_context_from("just a string", Cursor::new("STDIN")).unwrap();
        assert_eq!(resolved, "just a string");
    }

    #[test]
    fn resolve_context_file_path_still_reads_file() {
        let dir = tempdir().unwrap();
        let brief = dir.path().join("brief.md");
        std::fs::write(&brief, "file body").unwrap();
        let resolved = resolve_context_from(brief.to_str().unwrap(), Cursor::new("STDIN")).unwrap();
        assert_eq!(resolved, "file body");
    }

    #[test]
    fn resolve_stdin_context_dash_reads_stdin() {
        let body = "multi\nline\nbrief\n";
        let resolved = resolve_stdin_context_from(Some("-"), Cursor::new(body)).unwrap();
        assert_eq!(resolved.as_deref(), Some(body));
    }

    #[test]
    fn resolve_stdin_context_literal_unchanged_and_no_file_resolution() {
        let dir = tempdir().unwrap();
        let brief = dir.path().join("brief.md");
        std::fs::write(&brief, "file body").unwrap();
        // Unlike resolve_context, a file path is left as the literal string.
        let resolved =
            resolve_stdin_context_from(Some(brief.to_str().unwrap()), Cursor::new("STDIN"))
                .unwrap();
        assert_eq!(resolved.as_deref(), Some(brief.to_str().unwrap()));
    }

    #[test]
    fn resolve_stdin_context_none_is_none() {
        let resolved = resolve_stdin_context_from(None, Cursor::new("STDIN")).unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn feat_new_creates_all_resources() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        let before = Utc::now();

        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
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
        let main_path = paths::main_worktree(&project_path);
        assert!(git::branch_exists(&main_path, "login").unwrap());

        // Worktree directory exists
        let worktree_path = project_path.join("login");
        assert!(worktree_path.exists());
        assert!(worktree_path.is_dir());

        // Tmux session exists
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
    }

    #[test]
    fn feat_new_duplicate_name_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();
        let result = feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ));

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
        tmux::create_session(
            server.name(),
            &tmux::session_name(&project_name, "login"),
            dir.path(),
        )
        .unwrap();

        let result = feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ));
        assert!(result.is_err());

        // State file should be cleaned up
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));

        // Branch and worktree should be cleaned up
        let main_path = paths::main_worktree(&project_path);
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

        let result = feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ));
        assert!(result.is_err());

        // State file should be cleaned up
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));

        // Branch should be cleaned up (worktree was never created by git)
        let main_path = paths::main_worktree(&project_path);
        assert!(!git::branch_exists(&main_path, "login").unwrap());
    }

    #[test]
    fn feat_new_with_text_context_enqueues_message_to_auto_spawn_agents() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        feat_new(&FeatNewParams {
            context: Some("Implement login page per issue #42"),
            workflow: Some("implement-and-review"),
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        })
        .unwrap();

        // No TASK.md on disk any more.
        assert!(!project_path.join("login").join("TASK.md").exists());

        // The context is queued as a message in the implementer's inbox
        // (the sole auto_spawn agent of `implement-and-review`).
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

        feat_new(&FeatNewParams {
            context: Some(brief_path.to_str().unwrap()),
            workflow: Some("implement-and-review"),
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        })
        .unwrap();

        // No TASK.md; content is queued to the auto_spawn agent's inbox.
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

        feat_new(&FeatNewParams {
            context: Some(brief_path.to_str().unwrap()),
            workflow: Some("implement-and-review"),
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        })
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

        feat_new(&FeatNewParams {
            context: Some("Build the login page"),
            workflow: Some("implement-and-review"),
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        })
        .unwrap();

        // Session should have 2 windows: the reused window :0 (now agent) + hook window
        let output =
            tmux::list_windows(server.name(), &tmux::session_name(&project_name, "login")).unwrap();
        assert_eq!(output, 2);
    }

    #[test]
    fn feat_new_with_workflow_spawns_named_agent() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());

        feat_new(&FeatNewParams {
            context: Some("Build the login page"),
            workflow: Some("research-only"),
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        })
        .unwrap();

        // The researcher window should exist (research-only's sole auto_spawn agent)
        let session = tmux::session_name(&project_name, "login");
        let target = tmux::find_window(server.name(), &session, "researcher").unwrap();
        assert!(target.is_some(), "expected a 'researcher' tmux window");

        // The agent should be registered in the agent registry
        let agents_dir = paths::agents_dir(&project_path);
        let registry = crate::state::agent::AgentRegistry::load(&agents_dir, "login").unwrap();
        let entry = registry.get("researcher");
        assert!(entry.is_some(), "expected 'researcher' in agent registry");
    }

    #[test]
    fn feat_new_without_context_has_shell_and_hook_windows() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());

        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        // 2 windows: default shell + hook window
        let session = tmux::session_name(&project_name, "login");
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
        // No workflow either when --workflow not given.
        assert!(state.workflow.is_none());
    }

    #[test]
    fn feat_new_skips_hook_when_script_removed() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());

        // Remove the bootstrapped hook script
        std::fs::remove_file(project_path.join(hooks::POST_CREATE_PATH)).unwrap();

        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();

        // Only 1 window — hook was skipped because file is missing
        let windows =
            tmux::list_windows(server.name(), &tmux::session_name(&project_name, "login")).unwrap();
        assert_eq!(windows, 1);
    }

    #[test]
    fn feat_new_with_base_stores_base_in_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
        .unwrap();
        feat_new(&FeatNewParams {
            base: Some("login"),
            ..FeatNewParams::with_defaults(&project_path, "stacked", server.name())
        })
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
        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "parent",
            server.name(),
        ))
        .unwrap();
        let parent_wt = project_path.join("parent");
        std::fs::write(parent_wt.join("parent.txt"), "parent work").unwrap();
        git::stage_file(&parent_wt, "parent.txt").unwrap();
        git::commit(&parent_wt, "parent commit").unwrap();

        // Create stacked feature based on parent
        feat_new(&FeatNewParams {
            base: Some("parent"),
            ..FeatNewParams::with_defaults(&project_path, "child", server.name())
        })
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

        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "login",
            server.name(),
        ))
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
        let main_path = paths::main_worktree(&project_path);
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
        let main_path = paths::main_worktree(&project_path);
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
        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "ciaran-login",
            server.name(),
        ))
        .unwrap();

        // "ciaran/login" sanitizes to "ciaran-login" — should conflict
        let result = feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "ciaran/login",
            server.name(),
        ));
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

        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "ciaran/login",
            server.name(),
        ))
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
        assert!(
            tmux::has_session(
                server.name(),
                &tmux::session_name(&project_name, "ciaran-login")
            )
            .unwrap()
        );
    }

    #[test]
    fn feat_new_with_name_override() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());

        feat_new(&FeatNewParams {
            name_override: Some("eval"),
            ..FeatNewParams::with_defaults(&project_path, "ciaran/eval", server.name())
        })
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "eval").unwrap();
        assert_eq!(state.branch, "ciaran/eval");
        assert_eq!(state.worktree, "eval");
        assert!(project_path.join("eval").exists());
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "eval")).unwrap()
        );
    }

    #[test]
    fn feat_new_blocked_by_feature_limit() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        // Set max_features = 1
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config.project.max_features = Some(1);
        config.save(&pm_dir).unwrap();

        // Create first feature — should succeed
        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "first",
            server.name(),
        ))
        .unwrap();

        // Second feature should be blocked
        let result = feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "second",
            server.name(),
        ));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, PmError::SafetyCheck(_)));
        assert!(err.to_string().contains("Feature limit reached"));
    }

    #[test]
    fn feat_new_allowed_under_feature_limit() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        // Set max_features = 2
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config.project.max_features = Some(2);
        config.save(&pm_dir).unwrap();

        // First feature
        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "first",
            server.name(),
        ))
        .unwrap();

        // Second feature should also succeed (2/2 would block, but 1/2 is fine)
        feat_new(&FeatNewParams::with_defaults(
            &project_path,
            "second",
            server.name(),
        ))
        .unwrap();

        // Verify both exist
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "first"));
        assert!(FeatureState::exists(&features_dir, "second"));
    }

    #[test]
    fn feat_new_context_without_workflow_errors() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        let result = feat_new(&FeatNewParams {
            context: Some("do X"),
            workflow: None,
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("--context requires --workflow"));

        // No partial state should remain on disk.
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
        assert!(!project_path.join("login").exists());
    }

    #[test]
    fn feat_new_workflow_without_context_stores_workflow() {
        // `pm feat new my-feat --workflow X` (no --context) is valid: it
        // records the workflow in state but spawns nothing. Useful for
        // when the user wants to spawn the auto-spawn agent later.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        feat_new(&FeatNewParams {
            workflow: Some("implement-and-review"),
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        })
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.workflow.as_deref(), Some("implement-and-review"));

        // No agent spawned (no context).
        let agents_dir = paths::agents_dir(&project_path);
        let registry = crate::state::agent::AgentRegistry::load(&agents_dir, "login").unwrap();
        assert!(
            registry.get("implementer").is_none(),
            "no auto-spawn without --context"
        );
    }

    #[test]
    fn feat_new_nonexistent_workflow_errors() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        let result = feat_new(&FeatNewParams {
            context: Some("do X"),
            workflow: Some("does-not-exist"),
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        });
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::WorkflowNotFound(_)));

        // No partial state should remain on disk.
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn feat_new_workflow_with_empty_auto_spawn_and_context_errors() {
        // A workflow with no auto_spawn agents has nobody to deliver
        // --context to. Treat it the same as `--context` without
        // `--workflow`: hard error before any side effects.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        let workflows = paths::workflows_dir(&project_path).join("empty");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(workflows.join("config.toml"), "description = \"x\"\n").unwrap();
        std::fs::write(workflows.join("workflow.md"), "# empty\n").unwrap();

        let result = feat_new(&FeatNewParams {
            context: Some("do X"),
            workflow: Some("empty"),
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, PmError::SafetyCheck(_)));
        assert!(
            err.to_string().contains("empty `auto_spawn`"),
            "expected empty auto_spawn error, got: {err}",
        );

        // No partial state should remain.
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn feat_new_workflow_with_empty_auto_spawn_no_context_succeeds() {
        // Without --context, an empty auto_spawn is fine — pm just
        // records the workflow and spawns nothing.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        let workflows = paths::workflows_dir(&project_path).join("empty");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(workflows.join("config.toml"), "description = \"x\"\n").unwrap();
        std::fs::write(workflows.join("workflow.md"), "# empty\n").unwrap();

        feat_new(&FeatNewParams {
            workflow: Some("empty"),
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        })
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.workflow.as_deref(), Some("empty"));
    }

    #[test]
    fn feat_new_workflow_with_unknown_auto_spawn_agent_errors() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        // Inject a workflow whose auto_spawn references a missing agent.
        let workflows = paths::workflows_dir(&project_path).join("broken");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(
            workflows.join("config.toml"),
            "description = \"x\"\nauto_spawn = [\"ghost-impl\"]\n",
        )
        .unwrap();
        std::fs::write(workflows.join("workflow.md"), "# broken\n").unwrap();

        let result = feat_new(&FeatNewParams {
            context: Some("do X"),
            workflow: Some("broken"),
            ..FeatNewParams::with_defaults(&project_path, "login", server.name())
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PmError::WorkflowAgentMissing { .. }),
            "expected WorkflowAgentMissing, got: {err}"
        );

        // No partial state should remain.
        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
    }
}
