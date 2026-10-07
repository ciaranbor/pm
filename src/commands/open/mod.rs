//! `pm open`: recreating a project's missing tmux sessions and respawning
//! its active agents, after warning about drift open cannot restore itself.

mod drift;
mod respawn;

use drift::warn_about_drift;
use respawn::respawn_agents_for_scope;

use std::path::{Path, PathBuf};

use crate::commands::launch_check::{self, FailedLaunch, Launch};
use crate::error::{PmError, Result};
use crate::hooks;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};
use crate::tmux;

/// Result of an `open` operation, containing restore statistics.
pub struct OpenResult {
    /// Number of sessions that were created (not already existing).
    pub sessions_restored: usize,
    /// Number of agents that were successfully respawned.
    pub agents_respawned: usize,
    /// The project's main tmux session name, for attaching/switching the client.
    pub main_session: String,
    project_root: PathBuf,
    /// The respawned agents, whose launches [`confirm_launches`] checks.
    launched: Vec<Launch>,
    /// The respawned agents whose harness exited at launch or did not come
    /// up.
    pub failed_launches: Vec<FailedLaunch>,
    /// What the open skipped or failed to spawn.
    pub warnings: Vec<String>,
}

impl OpenResult {
    /// Whether the open made any session or agent.
    pub fn changed(&self) -> bool {
        self.sessions_restored > 0 || self.agents_respawned > 0
    }
}

/// Check the launches of every agent `results` respawned, together, and
/// move each that exited at launch or did not come up from its result's
/// `agents_respawned` to its `failed_launches`.
pub fn confirm_launches<'a>(
    results: impl IntoIterator<Item = &'a mut OpenResult>,
    tmux_server: Option<&str>,
) {
    let mut results: Vec<&mut OpenResult> = results.into_iter().collect();
    let launches: Vec<Launch> = results
        .iter_mut()
        .flat_map(|r| std::mem::take(&mut r.launched))
        .collect();
    for failure in launch_check::confirm_all(&launches, tmux_server) {
        if let Some(result) = results
            .iter_mut()
            .find(|r| r.project_root == failure.launch.project_root)
        {
            result.agents_respawned = result.agents_respawned.saturating_sub(1);
            result.failed_launches.push(failure);
        }
    }
}

/// Open a project: ensure all tmux sessions exist, then respawn agents.
///
/// Creates the `<project>/main` session if missing, then creates sessions for
/// any active features that are missing their sessions. Existing sessions are
/// left untouched (resurrect-aware).
///
/// Before doing any recreation, runs `pm doctor`'s diagnostic checks (without
/// fixing) and prints warnings to stderr for any drift that open cannot
/// auto-restore (orphaned state, missing branches, PR drift, missing hooks,
/// stuck-initializing features). Issues that open *will* fix automatically
/// (missing tmux sessions, dead agent windows) are filtered out.
///
/// After session creation, respawns agents marked `active = true` via
/// `agent_spawn_all`. Agents whose windows already exist are skipped.
///
/// Finally, selects a sensible landing window in each restored session: the
/// first agent window if any agents were respawned, otherwise window 0.
///
/// Features in `initializing` state are skipped — those represent incomplete
/// creations that `pm doctor` should handle.
///
/// Worktree directories that are missing on disk are skipped with a warning
/// in [`OpenResult::warnings`] rather than aborting the entire open.
///
/// The `tmux_server` parameter allows tests to use an isolated tmux server.
pub fn open(
    project_root: &Path,
    projects_dir: &Path,
    tmux_server: Option<&str>,
) -> Result<OpenResult> {
    open_project(project_root, projects_dir, tmux_server)
}

/// What `open_all` did with one registered project.
pub enum ProjectOpen {
    Opened(OpenResult),
    /// The registered root is not on disk.
    RootMissing(PathBuf),
    Failed(PmError),
}

/// [`open`] every project registered in `projects_dir`, in registry order,
/// passing each project's outcome to `report` as soon as it is known; each
/// outcome, by name, for the launches to be confirmed after.
///
/// A project whose root is missing, or whose open fails, is reported and the
/// sweep continues. The client is never switched or attached here; that is
/// the caller's choice.
pub fn open_all(
    projects_dir: &Path,
    tmux_server: Option<&str>,
    mut report: impl FnMut(&str, &ProjectOpen),
) -> Result<Vec<(String, ProjectOpen)>> {
    let mut outcomes = Vec::new();
    for (name, entry) in ProjectEntry::list(projects_dir)? {
        let root = entry.root_path();
        let outcome = if !root.exists() {
            ProjectOpen::RootMissing(root)
        } else {
            match open_project(&root, projects_dir, tmux_server) {
                Ok(result) => ProjectOpen::Opened(result),
                Err(e) => ProjectOpen::Failed(e),
            }
        };
        report(&name, &outcome);
        outcomes.push((name, outcome));
    }
    Ok(outcomes)
}

fn open_project(
    project_root: &Path,
    projects_dir: &Path,
    tmux_server: Option<&str>,
) -> Result<OpenResult> {
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Run doctor's diagnostic checks and warn about state drift before doing
    // any restoration. This surfaces issues like orphaned features or missing
    // branches that `pm open` cannot fix on its own.
    warn_about_drift(project_root, project_name, projects_dir, tmux_server);

    // Backfill hook scripts for projects created before lifecycle hooks existed
    hooks::bootstrap(project_root)?;

    let mut sessions_restored: usize = 0;
    let mut launched = Vec::new();
    let agents_dir = paths::agents_dir(project_root);

    // Ensure <project>/main session exists
    let main_session = tmux::session_name(project_name, "main");
    let restore_hook = project_root.join(hooks::RESTORE_PATH);
    if !tmux::has_session(tmux_server, &main_session)? {
        let main_path = paths::main_worktree(project_root);
        if !main_path.exists() {
            return Err(PmError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("main worktree missing: {}", main_path.display()),
            )));
        }
        tmux::create_session(tmux_server, &main_session, &main_path)?;
        hooks::run_hook(
            tmux_server,
            &hooks::HookContext::scope(project_root, project_name, "main"),
            &restore_hook,
        );
        sessions_restored += 1;
    }

    // Respawn agents marked active in the main scope. If the session was just
    // recreated, their windows are gone and agent_spawn will create new ones.
    // If the session already existed, agent_spawn is idempotent (skips agents
    // whose windows are still present).
    let mut warnings = respawn_agents_for_scope(
        project_root,
        "main",
        &main_session,
        &agents_dir,
        tmux_server,
        false,
        &mut launched,
    )?;

    // Ensure sessions exist for all active features
    let features_dir = paths::features_dir(project_root);
    let features = FeatureState::list(&features_dir)?;

    // Track which features have active sessions (for agent respawn)
    let mut active_features: Vec<String> = Vec::new();

    for (name, state) in &features {
        if !state.status.is_active() {
            continue;
        }
        let session_name = tmux::session_name(project_name, name);
        if !tmux::has_session(tmux_server, &session_name)? {
            let worktree_path = project_root.join(&state.worktree);
            if !worktree_path.exists() {
                warnings.push(format!(
                    "skipping '{name}': worktree missing at {}",
                    worktree_path.display()
                ));
                continue;
            }
            tmux::create_session(tmux_server, &session_name, &worktree_path)?;
            hooks::run_hook(
                tmux_server,
                &hooks::HookContext {
                    worktree: worktree_path.clone(),
                    ..hooks::HookContext::scope(project_root, project_name, name)
                },
                &restore_hook,
            );
            sessions_restored += 1;
        }
        active_features.push(name.clone());
    }

    // Respawn agents for ALL active features (not just restored sessions).
    // agent_spawn is idempotent — skips agents whose windows already exist.
    for feature in &active_features {
        let session_name = tmux::session_name(project_name, feature);
        warnings.extend(respawn_agents_for_scope(
            project_root,
            feature,
            &session_name,
            &agents_dir,
            tmux_server,
            true,
            &mut launched,
        )?);
    }

    Ok(OpenResult {
        sessions_restored,
        agents_respawned: launched.len(),
        main_session,
        project_root: project_root.to_path_buf(),
        launched,
        failed_launches: Vec::new(),
        warnings,
    })
}

#[cfg(test)]
mod tests;
