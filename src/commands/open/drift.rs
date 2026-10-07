//! The drift warnings `pm open` prints before restoring: `pm doctor`'s quick
//! findings, minus the issue kinds open restores on its own.

use std::path::Path;

use crate::commands::doctor::{self, Depth, IssueKind};
use crate::error::Result;

/// Returns true for issue kinds that `pm open` is about to fix automatically
/// (recreating tmux sessions and respawning agent windows). These are filtered
/// out of the pre-open warnings to avoid noisy output for normal restart flows.
///
/// Uses an exhaustive `match` (no `_` wildcard) so adding a new [`IssueKind`]
/// variant becomes a compile error, forcing the author to classify it as
/// open-recoverable or not.
fn is_open_recoverable(kind: IssueKind) -> bool {
    match kind {
        IssueKind::TmuxSessionMissing | IssueKind::AgentWindowMissing => true,
        IssueKind::OrphanedState
        | IssueKind::AgentSessionNotStarted
        | IssueKind::AgentHarnessExited
        | IssueKind::WorktreeDirMissing
        | IssueKind::DirNotGitWorktree
        | IssueKind::GitWorktreeNoDir
        | IssueKind::BranchMissing
        | IssueKind::StuckInitializing
        | IssueKind::WorkflowDirMissing
        | IssueKind::PrMerged
        | IssueKind::PrClosed
        | IssueKind::PrCheckFailed
        | IssueKind::HooksNotInstalled
        | IssueKind::StaleProjectHooks
        | IssueKind::AssetNotProjected
        | IssueKind::GlobalStoreMissing
        | IssueKind::StaleBundledCopies
        | IssueKind::RedundantOverride
        | IssueKind::SkillShadowedByGlobal
        | IssueKind::LegacyVanillaAgentName
        | IssueKind::LegacyVanillaConfigRow
        | IssueKind::HooksMalformed
        | IssueKind::HookUntrusted
        | IssueKind::WorktreeUntrusted
        | IssueKind::MainBranchMissing
        | IssueKind::HarnessUnusable
        | IssueKind::LoopStopped
        | IssueKind::LoopNotLoaded
        | IssueKind::TurnFailed
        | IssueKind::AgentModelMissing
        | IssueKind::AgentRowInvalid
        | IssueKind::AgentRowRemark
        | IssueKind::HarnessConfigInvalid
        | IssueKind::ProviderKeyUnset
        | IssueKind::ProviderUnreachable
        | IssueKind::RebaseInProgress
        | IssueKind::BundledDisabled
        | IssueKind::BundledUnknown
        | IssueKind::DisabledStillInstalled
        | IssueKind::BundledDisabledDangling
        | IssueKind::AgentLaunchStale => false,
    }
}

const FIXABLE_SUFFIX: &str = " [fixable: `pm doctor --fix`]";

/// Collect drift warnings to print before opening: doctor findings minus the
/// issue kinds that open will auto-restore.
///
/// PR drift checks are skipped (`check_pr_state = false`) to avoid making
/// `gh pr view` network calls on every `pm open` — that's a `pm doctor` job.
///
/// Returns lines of the form `"  <scope> — <message>"`, with
/// [`FIXABLE_SUFFIX`] on those `pm doctor --fix` resolves.
fn collect_drift_warnings(
    project_root: &Path,
    projects_dir: &Path,
    tmux_server: Option<&str>,
) -> Result<Vec<String>> {
    let findings = doctor::diagnose(project_root, projects_dir, tmux_server, Depth::Quick)?;
    let mut warnings: Vec<String> = Vec::new();
    for finding in &findings {
        for issue in finding.issues() {
            // `BundledDisabled` reports a deliberate setting, not drift.
            if is_open_recoverable(issue.kind()) || issue.kind() == IssueKind::BundledDisabled {
                continue;
            }
            let suffix = if issue.auto_fixable() {
                FIXABLE_SUFFIX
            } else {
                ""
            };
            warnings.push(format!(
                "  {} — {}{suffix}",
                finding.feature(),
                issue.message()
            ));
        }
    }
    Ok(warnings)
}

/// Run doctor's diagnostic checks and print warnings for any issues that
/// `pm open` cannot auto-restore. Used to surface state drift (orphaned
/// features, missing branches, PR drift, missing hooks, etc.) before
/// recreating tmux sessions.
///
/// Failures during diagnosis are themselves printed as warnings rather than
/// aborting open — diagnostics are best-effort, recovery is the priority.
pub(super) fn warn_about_drift(
    project_root: &Path,
    project_name: &str,
    projects_dir: &Path,
    tmux_server: Option<&str>,
) {
    let warnings = match collect_drift_warnings(project_root, projects_dir, tmux_server) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("warning: pre-open diagnostics failed: {e}");
            return;
        }
    };

    if warnings.is_empty() {
        return;
    }

    eprintln!("warning: pm doctor detected state drift in {project_name}:");
    for line in &warnings {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use crate::state::agent::AgentRegistry;
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::state::paths;
    use crate::testing::TestServer;
    use crate::tmux;
    use tempfile::tempdir;

    #[test]
    fn is_open_recoverable_filters_tmux_and_agents() {
        // open() recreates these on its own — they shouldn't appear as warnings
        assert!(is_open_recoverable(IssueKind::TmuxSessionMissing));
        assert!(is_open_recoverable(IssueKind::AgentWindowMissing));
        // Everything else needs human (or `pm doctor --fix`) attention
        assert!(!is_open_recoverable(IssueKind::OrphanedState));
        assert!(!is_open_recoverable(IssueKind::WorktreeDirMissing));
        assert!(!is_open_recoverable(IssueKind::DirNotGitWorktree));
        assert!(!is_open_recoverable(IssueKind::GitWorktreeNoDir));
        assert!(!is_open_recoverable(IssueKind::BranchMissing));
        assert!(!is_open_recoverable(IssueKind::StuckInitializing));
        assert!(!is_open_recoverable(IssueKind::PrMerged));
        assert!(!is_open_recoverable(IssueKind::PrClosed));
        assert!(!is_open_recoverable(IssueKind::PrCheckFailed));
        assert!(!is_open_recoverable(IssueKind::HooksNotInstalled));
    }

    #[test]
    fn drift_warnings_skip_missing_tmux_session() {
        // A killed tmux session is something open will recreate itself, so it
        // shouldn't appear as a drift warning.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

        let warnings = collect_drift_warnings(&project_path, &projects_dir, server.name()).unwrap();
        assert!(
            !warnings.iter().any(|w| w.contains("tmux session")),
            "tmux session warning should be filtered out, got: {warnings:?}"
        );
    }

    #[test]
    fn drift_warnings_report_orphaned_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Fully orphan the feature: remove worktree, branch, and session.
        let main_repo = paths::main_worktree(&project_path);
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        git::delete_branch(&main_repo, "login").unwrap();
        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

        let warnings = collect_drift_warnings(&project_path, &projects_dir, server.name()).unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("orphaned state file") && w.ends_with(FIXABLE_SUFFIX)),
            "expected orphaned-state warning, got: {warnings:?}"
        );
    }

    #[test]
    fn drift_warnings_do_not_offer_fix_for_an_agent_whose_session_never_started() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session_name, "login", "reviewer");
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        registry.get_mut("reviewer").unwrap().spawned_at =
            Some(chrono::Utc::now() - chrono::Duration::hours(1));
        registry.save(&agents_dir, "login").unwrap();
        std::fs::File::create(
            crate::state::runtime::reset_launched(&project_path, "login", "reviewer").unwrap(),
        )
        .unwrap();

        let warnings = collect_drift_warnings(&project_path, &projects_dir, server.name()).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].ends_with("(run `pm agent restart reviewer --scope login`)"),
            "{warnings:?}"
        );
    }

    #[test]
    fn drift_warnings_report_stuck_initializing() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.status = FeatureStatus::Initializing;
        state.save(&features_dir, "login").unwrap();

        let warnings = collect_drift_warnings(&project_path, &projects_dir, server.name()).unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("stuck on 'initializing'")),
            "expected stuck-initializing warning, got: {warnings:?}"
        );
    }

    #[test]
    fn drift_warnings_empty_for_healthy_project() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        let warnings = collect_drift_warnings(&project_path, &projects_dir, server.name()).unwrap();
        assert!(
            warnings.is_empty(),
            "healthy project should produce no warnings, got: {warnings:?}"
        );
    }
}
