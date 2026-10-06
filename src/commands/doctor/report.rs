//! [`doctor`]'s report, and applying each fix under `--fix`.

use std::path::Path;

use super::warnings::{
    baseline_capability_warnings, global_config_warning, registry_warnings, serve_warnings,
};
use super::{Depth, Fix, FixAction, diagnose};
use crate::commands::feat_delete::{self, CleanupParams};
use crate::commands::{agent_spawn, hooks_install};
use crate::error::Result;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};
use crate::{git, tmux};

/// One line of a [`Report`], for one scope.
struct Entry {
    /// Whether the line reports an issue rather than a healthy scope.
    issue: bool,
    line: String,
}

/// The outcome of a [`doctor`] run.
pub struct Report {
    feature_count: usize,
    entries: Vec<Entry>,
    fixed_count: usize,
    fix: bool,
    warnings: Vec<String>,
}

impl Report {
    /// Issues found, fixed ones included.
    pub fn issue_count(&self) -> usize {
        self.entries.iter().filter(|e| e.issue).count()
    }

    /// One line per issue found, naming its scope and, under `--fix`, what
    /// became of it.
    pub fn issue_lines(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|e| e.issue)
            .map(|e| e.line.as_str())
    }

    /// Project-independent warnings.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The full report: a summary line, a line per scope or issue, then the
    /// warnings.
    pub fn lines(&self) -> Vec<String> {
        let checked = format!("Checked main and {} feature(s)", self.feature_count);
        let total = self.issue_count();
        let summary = if total == 0 {
            format!("{checked}: all healthy")
        } else if self.fix && self.fixed_count > 0 {
            format!(
                "{checked}: {total} issue(s) found, {} fixed",
                self.fixed_count
            )
        } else {
            format!("{checked}: {total} issue(s) found")
        };
        std::iter::once(summary)
            .chain(self.entries.iter().map(|e| e.line.clone()))
            .chain(self.warnings.iter().cloned())
            .collect()
    }
}

/// Run a health check on all features in the project.
///
/// Wraps [`diagnose`] with reporting and (optionally) auto-fix logic.
/// Runs at [`Depth::Full`], so PR drift is reported here.
///
/// With `fix == true`, auto-resolves clear-cut issues and skips ambiguous ones.
pub fn doctor(
    project_root: &Path,
    projects_dir: &Path,
    fix: bool,
    tmux_server: Option<&str>,
) -> Result<Report> {
    run(project_root, projects_dir, fix, tmux_server, Depth::Full)
}

/// [`doctor`] without fixes, at [`Depth::Quick`].
pub fn offline(
    project_root: &Path,
    projects_dir: &Path,
    tmux_server: Option<&str>,
) -> Result<Report> {
    run(project_root, projects_dir, false, tmux_server, Depth::Quick)
}

fn run(
    project_root: &Path,
    projects_dir: &Path,
    fix: bool,
    tmux_server: Option<&str>,
    depth: Depth,
) -> Result<Report> {
    let mut warnings = baseline_capability_warnings(project_root, depth.probe())?;
    warnings.extend(global_config_warning());
    warnings.extend(registry_warnings(projects_dir)?);
    warnings.extend(serve_warnings(depth));

    let findings = diagnose(project_root, projects_dir, tmux_server, depth)?;
    let feature_count = FeatureState::list(&paths::features_dir(project_root))?.len();

    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let mut entries = Vec::new();
    let mut fixed_count = 0;

    for finding in &findings {
        if finding.issues.is_empty() {
            entries.push(Entry {
                issue: false,
                line: format!("  {} — ok", finding.feature),
            });
            continue;
        }

        for issue in &finding.issues {
            let line = match &issue.fix {
                Fix::Auto(action) if fix => match apply_fix(
                    action,
                    project_root,
                    projects_dir,
                    &finding.feature,
                    project_name,
                    tmux_server,
                ) {
                    Ok(notes) => {
                        fixed_count += 1;
                        format!(
                            "  {} — fixed: {}{}",
                            finding.feature,
                            issue.message,
                            agent_spawn::notes_suffix(&notes)
                        )
                    }
                    Err(e) => format!(
                        "  {} — fix failed ({}): {}",
                        finding.feature, e, issue.message
                    ),
                },
                Fix::Skip if fix => format!(
                    "  {} — skipped (ambiguous): {}",
                    finding.feature, issue.message
                ),
                _ => format!("  {} — {}", finding.feature, issue.message),
            };
            entries.push(Entry { issue: true, line });
        }
    }

    Ok(Report {
        feature_count,
        entries,
        fixed_count,
        fix,
        warnings,
    })
}

/// Apply a single fix action. Returns the notes a respawn or a cleanup
/// produced; empty for every other action.
fn apply_fix(
    action: &FixAction,
    project_root: &Path,
    projects_dir: &Path,
    name: &str,
    project_name: &str,
    tmux_server: Option<&str>,
) -> Result<Vec<String>> {
    let features_dir = &paths::features_dir(project_root);
    let main_repo = &paths::main_worktree(project_root);
    match action {
        FixAction::RemoveState => {
            FeatureState::delete(features_dir, name)?;
        }
        FixAction::CleanupInitializing {
            worktree,
            branch,
            base_scope,
        } => {
            let worktree_path = project_root.join(worktree);
            return feat_delete::cleanup_feature(&CleanupParams {
                repo: main_repo,
                worktree_path: &worktree_path,
                branch,
                features_dir,
                name,
                project_name,
                force_worktree: true,
                worktree_created: false,
                tmux_server,
                kill_session: true,
                delete_branch: true,
                best_effort: false,
                base_scope,
                ending: None,
            });
        }
        FixAction::RecreateTmuxSession {
            session_name,
            worktree_path,
        } => {
            tmux::create_session(tmux_server, session_name, worktree_path)?;
        }
        FixAction::UpdateStatus { new_status } => {
            let mut state = FeatureState::load(features_dir, name)?;
            state.status = *new_status;
            state.save(features_dir, name)?;
        }
        FixAction::RecreateWorktree {
            worktree_path,
            branch,
        } => {
            git::add_worktree(main_repo, worktree_path, branch)?;
        }
        FixAction::InstallStopHook => {
            hooks_install::install(Some(project_root))?;
        }
        FixAction::RecordMainBranch { branch } => {
            let mut entry = ProjectEntry::load(projects_dir, project_name)?;
            entry.main_branch = branch.clone();
            entry.save(projects_dir, project_name)?;
        }
        FixAction::InstallGlobalAssets => {
            crate::commands::skills::install_global()?;
        }
        FixAction::MigrateVanillaAgents => {
            crate::commands::vanilla_rename::migrate(project_root, false)?;
        }
        FixAction::RespawnAgent { agent_name } => {
            let (_, _, notes) =
                agent_spawn::agent_spawn(project_root, name, agent_name, None, None, tmux_server)?;
            return Ok(notes);
        }
        FixAction::TrustWorktree { harness, path } => {
            harness.trust_worktree(&paths::home_dir()?, path)?;
        }
        FixAction::PullFeatureAssets => {
            crate::commands::seed::pull(project_root, name, false)?;
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {

    use crate::state::feature::{FeatureState, FeatureStatus};

    use super::*;

    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn fix_recreates_missing_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let session_name = tmux::session_name(&project_name, "login");

        tmux::kill_session(server.name(), &session_name).unwrap();
        assert!(!tmux::has_session(server.name(), &session_name).unwrap());

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("tmux session")),
            "got: {lines:?}"
        );
        assert!(tmux::has_session(server.name(), &session_name).unwrap());
    }

    #[test]
    fn fix_cleans_up_stuck_initializing() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.status = FeatureStatus::Initializing;
        state.save(&features_dir, "login").unwrap();

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("initializing")),
            "got: {lines:?}"
        );
        // State file should be removed
        assert!(!FeatureState::exists(&features_dir, "login"));
        // Worktree directory should be removed
        assert!(!project_path.join("login").exists());
        // Branch should be removed
        let main_repo = paths::main_worktree(&project_path);
        assert!(!git::branch_exists(&main_repo, "login").unwrap());
        // Tmux session should be removed
        assert!(
            !tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
    }

    #[test]
    fn fix_removes_orphaned_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Remove worktree, branch, and tmux session — leaving only the state file
        let main_repo = paths::main_worktree(&project_path);
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        git::delete_branch(&main_repo, "login").unwrap();
        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("orphaned")),
            "got: {lines:?}"
        );
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn fix_recreates_missing_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Remove worktree directory — branch still exists, so doctor can recreate
        let main_repo = paths::main_worktree(&project_path);
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        assert!(!project_path.join("login").exists());

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("worktree directory missing")),
            "got: {lines:?}"
        );
        // Worktree should be recreated
        assert!(project_path.join("login").exists());
    }

    #[test]
    fn fix_summary_shows_fixed_count() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines[0].contains("fixed"),
            "summary should mention fixed count, got: {:?}",
            lines[0]
        );
    }
}
