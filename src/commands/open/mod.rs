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
use crate::state::project::{ProjectConfig, ProjectEntry};
use crate::state::{paths, scope};
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
/// Creates the session of every active scope that is missing one — main is
/// always active, and a missing main worktree is fatal. Existing sessions are
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
/// Each session that had agents respawned lands on its first agent window.
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
    /// The project is not on this machine.
    NotHere,
    Failed(PmError),
}

/// [`open`] every project registered in `projects_dir`, in registry order,
/// passing each project's outcome to `report` as soon as it is known; each
/// outcome, by name, for the launches to be confirmed after.
///
/// A project that isn't on this machine, or whose open fails, is reported
/// and the sweep continues. The client is never switched or attached here;
/// that is the caller's choice.
pub fn open_all(
    projects_dir: &Path,
    tmux_server: Option<&str>,
    mut report: impl FnMut(&str, &ProjectOpen),
) -> Result<Vec<(String, ProjectOpen)>> {
    let mut outcomes = Vec::new();
    for (name, entry) in ProjectEntry::list(projects_dir)? {
        let root = entry.root_path();
        let outcome = if !entry.presence().is_here() {
            ProjectOpen::NotHere
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
    let mut warnings = Vec::new();
    let agents_dir = paths::agents_dir(project_root);
    let restore_hook = project_root.join(hooks::RESTORE_PATH);

    for scope in scope::names(project_root)? {
        let Some(worktree) = scope::active_worktree(project_root, &scope)? else {
            continue;
        };
        let session_name = tmux::session_name(project_name, &scope);
        if !tmux::has_session(tmux_server, &session_name)? {
            if !worktree.exists() {
                if scope == scope::MAIN {
                    return Err(PmError::WorktreeMissing {
                        scope: scope.clone(),
                        path: worktree,
                    });
                }
                warnings.push(format!(
                    "skipping '{scope}': worktree missing at {}",
                    worktree.display()
                ));
                continue;
            }
            tmux::create_session(tmux_server, &session_name, &worktree)?;
            hooks::run_hook(
                tmux_server,
                &hooks::HookContext {
                    worktree,
                    ..hooks::HookContext::scope(project_root, project_name, &scope)
                },
                &restore_hook,
            );
            sessions_restored += 1;
        }
        // agent_spawn skips agents whose windows still exist, so a session
        // that survived only gets its dead agents back.
        warnings.extend(respawn_agents_for_scope(
            project_root,
            &scope,
            &session_name,
            &agents_dir,
            tmux_server,
            &mut launched,
        )?);
    }

    Ok(OpenResult {
        sessions_restored,
        agents_respawned: launched.len(),
        main_session: tmux::session_name(project_name, scope::MAIN),
        project_root: project_root.to_path_buf(),
        launched,
        failed_launches: Vec::new(),
        warnings,
    })
}

#[cfg(test)]
mod tests;
