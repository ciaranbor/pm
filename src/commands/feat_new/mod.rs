use std::path::Path;

use chrono::Utc;

use crate::commands::feat_common::{self, InitStateFields};
use crate::commands::feat_delete;
use crate::commands::seed;
use crate::error::{PmError, Result};
use crate::hooks;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};
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

/// Resolve the base branch for a new feature: the explicit `base`, else the
/// branch checked out at `cwd` (the main worktree when `cwd` is outside the
/// project), else `main_branch` when HEAD there is detached or not a repo.
pub fn resolve_base(
    project_root: &Path,
    main_branch: &str,
    base: Option<&str>,
    cwd: &Path,
) -> Result<String> {
    if let Some(b) = base {
        return Ok(b.to_string());
    }
    let main_worktree = paths::main_worktree(project_root);
    let detect_from = if cwd.starts_with(project_root) {
        cwd
    } else {
        main_worktree.as_path()
    };
    Ok(git::head_branch(detect_from).unwrap_or_else(|_| main_branch.to_string()))
}

/// Parameters for creating a new feature.
pub struct FeatNewParams<'a> {
    pub project_root: &'a Path,
    /// The project registry, holding the project's `main_branch`.
    pub projects_dir: &'a Path,
    pub name: &'a str,
    pub name_override: Option<&'a str>,
    pub context: Option<&'a str>,
    /// Which branch to stack on. When `None`, the current branch is detected
    /// from CWD (enabling natural stacking from within a feature worktree).
    pub base: Option<&'a str>,
    /// Workflow to activate for this feature. When `None` and `context` is
    /// provided, defaults to `feat_common::DEFAULT_WORKFLOW` (a context
    /// needs a recipient).
    pub workflow: Option<&'a str>,
    /// Allows tests to use an isolated tmux server. Pass `None` in production.
    pub tmux_server: Option<&'a str>,
}

#[cfg(test)]
impl<'a> FeatNewParams<'a> {
    /// Test helper: build params with all optional fields set to defaults.
    pub fn with_defaults(
        project_root: &'a Path,
        projects_dir: &'a Path,
        name: &'a str,
        tmux_server: Option<&'a str>,
    ) -> Self {
        Self {
            project_root,
            projects_dir,
            name,
            name_override: None,
            context: None,
            base: None,
            workflow: None,
            tmux_server,
        }
    }
}

/// Create a new feature: branch + worktree + tmux session + state file.
pub fn feat_new(params: &FeatNewParams<'_>) -> Result<String> {
    // Resolve the effective workflow (explicit wins; a context with no
    // workflow defaults to `solo`) — fail early before any side effects.
    let workflow =
        feat_common::resolve_workflow(params.project_root, params.workflow, params.context)?;

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
    super::feat_summary::ensure_no_untriaged(params.project_root, &feature_name)?;

    // Load project config for name
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Load the workflow definition (if any) and validate its team and
    // brief agents up-front. Fail early — before any tmux/git/state side
    // effects — so a bad workflow never leaves a half-built feature on
    // disk.
    let workflow_def = workflow
        .map(|w| feat_common::load_and_validate_workflow(params.project_root, w))
        .transpose()?;
    // The full team is spawned; the brief subset receives --context.
    let team: &[String] = workflow_def
        .as_ref()
        .map(|d| d.effective_team())
        .unwrap_or(&[]);
    let brief_agents: &[String] = workflow_def
        .as_ref()
        .map(|d| d.brief_agents.as_slice())
        .unwrap_or(&[]);

    // If the user supplied --context, somebody has to receive it.
    // A workflow with empty `brief_agents` would silently swallow the
    // context — block that case so the user gets a clear error.
    if params.context.is_some() && brief_agents.is_empty() {
        return Err(PmError::SafetyCheck(format!(
            "workflow '{}' has an empty `brief_agents` list, so --context has no recipient. \
             Add at least one agent to `brief_agents` in the workflow's config.toml, \
             or drop --context.",
            workflow.unwrap_or("<none>"),
        )));
    }

    // Resolve context upfront (file contents or literal text)
    let resolved_context = params.context.map(resolve_context).transpose()?;

    // Resolve base branch (explicit, or detected from CWD)
    let main_branch = ProjectEntry::load(params.projects_dir, project_name)?.main_branch;
    let cwd = std::env::current_dir()?;
    let resolved_base = resolve_base(params.project_root, &main_branch, params.base, &cwd)?;

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
            workflow,
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

        // Step 3.5: Seed harness assets and settings from main worktree
        seed::seed_feature_assets(params.project_root, &worktree_path)?;

        // Step 3.6: Enqueue initial context as a message to every
        // brief_agents agent in the workflow (if context provided). pm's
        // waiter wakes each agent with it once its session starts. TASK.md
        // is never written.
        if let Some(ref resolved) = resolved_context {
            feat_common::enqueue_initial_context(
                params.project_root,
                &feature_name,
                brief_agents,
                resolved,
            )?;
        }

        // Step 4: Create tmux session
        tmux::create_session(params.tmux_server, &session_name, &worktree_path)?;

        // Step 4.5: Spawn the workflow's full agent team (if any), with or
        // without --context. The first agent reuses window :0 so we don't
        // leave an empty default shell behind; subsequent agents go into
        // fresh windows.
        if !team.is_empty() {
            let reuse_target = format!("{session_name}:0");
            feat_common::spawn_team(
                params.project_root,
                &feature_name,
                team,
                Some(&reuse_target),
                params.tmux_server,
            )?;
        }

        // Step 4.6: Run post-create hook in a named "hook" window (non-fatal)
        hooks::run_hook(
            params.tmux_server,
            &hooks::HookContext::scope(params.project_root, project_name, &feature_name),
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
            base_scope: &feat_delete::base_scope(params.project_root, &main_branch, &resolved_base),
        });
        return Err(e);
    }

    Ok(feature_name)
}

#[cfg(test)]
mod tests;
