use std::path::{Path, PathBuf};

use crate::commands::feat_delete::{self, CleanupParams};
use crate::error::Result;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{gh, git, tmux};

/// A single issue detected for a feature.
struct Issue {
    message: String,
    fix: Fix,
}

/// What --fix should do about this issue.
enum Fix {
    /// Can be auto-resolved.
    Auto(FixAction),
    /// Ambiguous — skip with a message.
    Skip,
    /// Nothing to fix (informational).
    None,
}

enum FixAction {
    /// Remove the state file (orphaned feature).
    RemoveState,
    /// Clean up a stuck-initializing feature via cleanup_feature.
    CleanupInitializing { worktree: String, branch: String },
    /// Recreate a missing tmux session.
    RecreateTmuxSession {
        session_name: String,
        worktree_path: PathBuf,
    },
    /// Update feature status to match GH PR state.
    UpdateStatus { new_status: FeatureStatus },
}

/// Diagnostic finding for a single feature.
struct Finding {
    feature: String,
    issues: Vec<Issue>,
}

/// Run a health check on all features in the project.
///
/// For each feature, checks:
/// 1. Worktree directory exists on disk
/// 2. Git worktree list includes it
/// 3. Branch exists locally
/// 4. Tmux session exists
/// 5. Status stuck on "initializing"
/// 6. If PR linked, check GH status and update state if merged/closed
///
/// With `fix == true`, auto-resolves clear-cut issues and skips ambiguous ones.
///
/// Returns formatted diagnostic lines.
pub fn doctor(project_root: &Path, fix: bool, tmux_server: Option<&str>) -> Result<Vec<String>> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let features = FeatureState::list(&features_dir)?;
    if features.is_empty() {
        return Ok(vec!["No features to check".to_string()]);
    }

    let main_repo = project_root.join("main");
    let worktrees = git::list_worktrees(&main_repo)?;

    let mut findings: Vec<Finding> = Vec::new();

    for (name, state) in &features {
        let mut issues = Vec::new();

        let worktree_path = project_root.join(&state.worktree);

        // Check 1: worktree directory exists on disk
        let dir_exists = worktree_path.exists();

        // Check 2: git worktree list includes it
        let in_git_worktrees = if let Ok(canonical) = worktree_path.canonicalize() {
            worktrees
                .iter()
                .any(|w| Path::new(w).canonicalize().ok().as_ref() == Some(&canonical))
        } else {
            let wt_str = worktree_path.to_string_lossy();
            worktrees.iter().any(|w| w.ends_with(wt_str.as_ref()))
        };

        // Check 3: branch exists locally
        let branch_exists = git::branch_exists(&main_repo, &state.branch)?;

        // Detect orphaned state: no directory, no branch, not initializing.
        // Report as a single issue instead of redundant individual checks.
        if !dir_exists && !branch_exists && state.status != FeatureStatus::Initializing {
            issues.push(Issue {
                message: "orphaned state file (no worktree, no branch)".to_string(),
                fix: Fix::Auto(FixAction::RemoveState),
            });

            findings.push(Finding {
                feature: name.clone(),
                issues,
            });
            continue;
        }

        // Report individual check failures (non-orphan)
        if !dir_exists {
            issues.push(Issue {
                message: "worktree directory missing from disk".to_string(),
                fix: Fix::Skip,
            });
        }
        if dir_exists && !in_git_worktrees {
            issues.push(Issue {
                message: "directory exists but not registered as git worktree".to_string(),
                fix: Fix::Skip,
            });
        }
        if !dir_exists && in_git_worktrees {
            issues.push(Issue {
                message: "registered as git worktree but directory missing".to_string(),
                fix: Fix::Skip,
            });
        }
        if !branch_exists {
            issues.push(Issue {
                message: format!("branch '{}' not found", state.branch),
                fix: Fix::Skip,
            });
        }

        // Check 4: tmux session exists (only for active features)
        if state.status.is_active() {
            let session_name = format!("{project_name}/{name}");
            if !tmux::has_session(tmux_server, &session_name)? {
                let fix_action = if dir_exists {
                    Fix::Auto(FixAction::RecreateTmuxSession {
                        session_name: session_name.clone(),
                        worktree_path: worktree_path.clone(),
                    })
                } else {
                    Fix::Skip
                };
                issues.push(Issue {
                    message: format!("tmux session '{session_name}' missing"),
                    fix: fix_action,
                });
            }
        }

        // Check 5: stuck on initializing
        if state.status == FeatureStatus::Initializing {
            issues.push(Issue {
                message: "status stuck on 'initializing' (incomplete creation)".to_string(),
                fix: Fix::Auto(FixAction::CleanupInitializing {
                    worktree: state.worktree.clone(),
                    branch: state.branch.clone(),
                }),
            });
        }

        // Check 6: PR status drift
        if !state.pr.is_empty() {
            match gh::pr_state(&main_repo, &state.pr) {
                Ok(gh_state) => {
                    let gh_state = gh_state.to_uppercase();
                    match gh_state.as_str() {
                        "MERGED" if state.status != FeatureStatus::Merged => {
                            issues.push(Issue {
                                message: format!(
                                    "PR #{} is merged but status is '{}'",
                                    state.pr, state.status
                                ),
                                fix: Fix::Auto(FixAction::UpdateStatus {
                                    new_status: FeatureStatus::Merged,
                                }),
                            });
                        }
                        "CLOSED" if state.status.is_active() => {
                            issues.push(Issue {
                                message: format!(
                                    "PR #{} is closed but status is '{}'",
                                    state.pr, state.status
                                ),
                                fix: Fix::Auto(FixAction::UpdateStatus {
                                    new_status: FeatureStatus::Stale,
                                }),
                            });
                        }
                        _ => {}
                    }
                }
                Err(_) => {
                    issues.push(Issue {
                        message: format!("could not check PR #{} (gh CLI failed)", state.pr),
                        fix: Fix::None,
                    });
                }
            }
        }

        findings.push(Finding {
            feature: name.clone(),
            issues,
        });
    }

    let mut lines = Vec::new();
    let mut total_issues = 0;
    let mut fixed_count = 0;

    for finding in &findings {
        if finding.issues.is_empty() {
            lines.push(format!("  {} — ok", finding.feature));
            continue;
        }

        total_issues += finding.issues.len();

        if fix {
            for issue in &finding.issues {
                match &issue.fix {
                    Fix::Auto(action) => {
                        match apply_fix(
                            action,
                            project_root,
                            &features_dir,
                            &main_repo,
                            &finding.feature,
                            project_name,
                            tmux_server,
                        ) {
                            Ok(()) => {
                                lines.push(format!(
                                    "  {} — fixed: {}",
                                    finding.feature, issue.message
                                ));
                                fixed_count += 1;
                            }
                            Err(e) => {
                                lines.push(format!(
                                    "  {} — fix failed ({}): {}",
                                    finding.feature, e, issue.message
                                ));
                            }
                        }
                    }
                    Fix::Skip => {
                        lines.push(format!(
                            "  {} — skipped (ambiguous): {}",
                            finding.feature, issue.message
                        ));
                    }
                    Fix::None => {
                        lines.push(format!("  {} — {}", finding.feature, issue.message));
                    }
                }
            }
        } else {
            for issue in &finding.issues {
                lines.push(format!("  {} — {}", finding.feature, issue.message));
            }
        }
    }

    let summary = if total_issues == 0 {
        format!("Checked {} feature(s): all healthy", findings.len())
    } else if fix && fixed_count > 0 {
        format!(
            "Checked {} feature(s): {} issue(s) found, {} fixed",
            findings.len(),
            total_issues,
            fixed_count
        )
    } else {
        format!(
            "Checked {} feature(s): {} issue(s) found",
            findings.len(),
            total_issues
        )
    };
    lines.insert(0, summary);

    Ok(lines)
}

/// Apply a single fix action.
fn apply_fix(
    action: &FixAction,
    project_root: &Path,
    features_dir: &Path,
    main_repo: &Path,
    name: &str,
    project_name: &str,
    tmux_server: Option<&str>,
) -> Result<()> {
    match action {
        FixAction::RemoveState => {
            FeatureState::delete(features_dir, name)?;
        }
        FixAction::CleanupInitializing { worktree, branch } => {
            let worktree_path = project_root.join(worktree);
            feat_delete::cleanup_feature(&CleanupParams {
                main_repo,
                worktree_path: &worktree_path,
                branch,
                features_dir,
                name,
                project_name,
                force_worktree: true,
                tmux_server,
            })?;
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn setup_project(dir: &Path, server: &TestServer) -> std::path::PathBuf {
        let project_path = dir.join("myapp");
        let projects_dir = dir.join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        project_path
    }

    fn setup_project_with_feature(
        dir: &Path,
        feature_name: &str,
        server: &TestServer,
    ) -> std::path::PathBuf {
        let project_path = setup_project(dir, server);
        feat_new::feat_new(&project_path, feature_name, None, server.name()).unwrap();
        project_path
    }

    #[test]
    fn healthy_feature_reports_ok() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(lines[0].contains("all healthy"), "got: {:?}", lines);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("login") && l.contains("ok"))
        );
    }

    #[test]
    fn no_features_reports_empty() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert_eq!(lines, vec!["No features to check"]);
    }

    #[test]
    fn missing_worktree_directory_detected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Remove directory on disk without telling git — simulates real drift
        std::fs::remove_dir_all(project_path.join("login")).unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("worktree directory missing")),
            "got: {lines:?}"
        );
        // Also detects the git worktree registration mismatch
        assert!(
            lines
                .iter()
                .any(|l| l.contains("registered as git worktree but directory missing")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn directory_exists_but_not_git_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Deregister worktree from git but leave directory on disk
        let main_repo = project_path.join("main");
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        std::fs::create_dir_all(project_path.join("login")).unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("not registered as git worktree")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn missing_branch_detected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Remove worktree first (branch can't be deleted while checked out), then branch
        let main_repo = project_path.join("main");
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        git::delete_branch(&main_repo, "login").unwrap();
        // Re-create the directory so the only issue is the missing branch
        std::fs::create_dir_all(project_path.join("login")).unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            lines.iter().any(|l| l.contains("branch 'login' not found")),
            "got: {lines:?}"
        );
        // Should not report worktree directory missing
        assert!(
            !lines
                .iter()
                .any(|l| l.contains("worktree directory missing")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn missing_tmux_session_detected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Kill the feature's tmux session
        tmux::kill_session(server.name(), "myapp/login").unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("tmux session 'myapp/login' missing")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn stuck_initializing_detected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Manually set the feature status to initializing
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.status = FeatureStatus::Initializing;
        state.save(&features_dir, "login").unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            lines.iter().any(|l| l.contains("stuck on 'initializing'")),
            "got: {lines:?}"
        );
    }

    // Check 6 (PR state drift) is not unit-tested because it requires `gh` CLI
    // authenticated against a real GitHub remote. The logic is exercised via the
    // gh::pr_state wrapper; integration testing would need a mock or real repo.

    #[test]
    fn multiple_features_all_checked() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project(dir.path(), &server);
        feat_new::feat_new(&project_path, "alpha", None, server.name()).unwrap();
        feat_new::feat_new(&project_path, "beta", None, server.name()).unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(lines[0].contains("2 feature(s)"), "got: {:?}", lines);
        assert!(lines.iter().any(|l| l.contains("alpha")));
        assert!(lines.iter().any(|l| l.contains("beta")));
    }

    #[test]
    fn multiple_issues_on_same_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Remove worktree + branch + tmux session — fully orphaned
        let main_repo = project_path.join("main");
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        git::delete_branch(&main_repo, "login").unwrap();
        tmux::kill_session(server.name(), "myapp/login").unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        // Orphan is reported as a single consolidated issue
        assert!(
            lines.iter().any(|l| l.contains("orphaned state file")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn multiple_issues_non_orphan() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Remove only the worktree directory — branch still exists, so not orphaned
        std::fs::remove_dir_all(project_path.join("login")).unwrap();
        tmux::kill_session(server.name(), "myapp/login").unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        let issue_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.contains("login") && !l.contains("ok"))
            .collect();
        assert!(
            issue_lines.len() >= 2,
            "expected at least 2 issues, got: {issue_lines:?}"
        );
    }

    // --- --fix tests ---

    #[test]
    fn fix_recreates_missing_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        tmux::kill_session(server.name(), "myapp/login").unwrap();
        assert!(!tmux::has_session(server.name(), "myapp/login").unwrap());

        let lines = doctor(&project_path, true, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("tmux session")),
            "got: {lines:?}"
        );
        assert!(tmux::has_session(server.name(), "myapp/login").unwrap());
    }

    #[test]
    fn fix_cleans_up_stuck_initializing() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.status = FeatureStatus::Initializing;
        state.save(&features_dir, "login").unwrap();

        let lines = doctor(&project_path, true, server.name()).unwrap();
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
        let main_repo = project_path.join("main");
        assert!(!git::branch_exists(&main_repo, "login").unwrap());
        // Tmux session should be removed
        assert!(!tmux::has_session(server.name(), "myapp/login").unwrap());
    }

    #[test]
    fn fix_removes_orphaned_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Remove worktree, branch, and tmux session — leaving only the state file
        let main_repo = project_path.join("main");
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        git::delete_branch(&main_repo, "login").unwrap();
        tmux::kill_session(server.name(), "myapp/login").unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));

        let lines = doctor(&project_path, true, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("orphaned")),
            "got: {lines:?}"
        );
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn fix_skips_ambiguous_issues() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Remove only the directory — branch and state still exist, ambiguous
        std::fs::remove_dir_all(project_path.join("login")).unwrap();

        let lines = doctor(&project_path, true, server.name()).unwrap();
        assert!(
            lines.iter().any(|l| l.contains("skipped (ambiguous)")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn fix_summary_shows_fixed_count() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        tmux::kill_session(server.name(), "myapp/login").unwrap();

        let lines = doctor(&project_path, true, server.name()).unwrap();
        assert!(
            lines[0].contains("fixed"),
            "summary should mention fixed count, got: {:?}",
            lines[0]
        );
    }
}
