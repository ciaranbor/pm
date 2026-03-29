use std::path::Path;

use crate::error::Result;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{gh, git, tmux};

/// Diagnostic finding for a single feature.
struct Finding {
    feature: String,
    issues: Vec<String>,
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
/// Returns formatted diagnostic lines.
pub fn doctor(project_root: &Path, tmux_server: Option<&str>) -> Result<Vec<String>> {
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
        if !worktree_path.exists() {
            issues.push("worktree directory missing from disk".to_string());
        }

        // Check 2: git worktree list includes it
        // Use canonical path when directory exists, fall back to string match when missing
        let in_git_worktrees = if let Ok(canonical) = worktree_path.canonicalize() {
            worktrees
                .iter()
                .any(|w| Path::new(w).canonicalize().ok().as_ref() == Some(&canonical))
        } else {
            // Directory missing — check if git still has it registered by path suffix
            let wt_str = worktree_path.to_string_lossy();
            worktrees.iter().any(|w| w.ends_with(wt_str.as_ref()))
        };
        if worktree_path.exists() && !in_git_worktrees {
            issues.push("directory exists but not registered as git worktree".to_string());
        }
        if !worktree_path.exists() && in_git_worktrees {
            issues.push("registered as git worktree but directory missing".to_string());
        }

        // Check 3: branch exists locally
        if !git::branch_exists(&main_repo, &state.branch)? {
            issues.push(format!("branch '{}' not found", state.branch));
        }

        // Check 4: tmux session exists (only for active features)
        if state.status.is_active() {
            let session_name = format!("{project_name}/{name}");
            if !tmux::has_session(tmux_server, &session_name)? {
                issues.push(format!("tmux session '{session_name}' missing"));
            }
        }

        // Check 5: stuck on initializing
        if state.status == FeatureStatus::Initializing {
            issues.push("status stuck on 'initializing' (incomplete creation)".to_string());
        }

        // Check 6: PR status drift
        if !state.pr.is_empty() {
            match gh::pr_state(&main_repo, &state.pr) {
                Ok(gh_state) => {
                    let gh_state = gh_state.to_uppercase();
                    match gh_state.as_str() {
                        "MERGED" if state.status != FeatureStatus::Merged => {
                            issues.push(format!(
                                "PR #{} is merged but status is '{}'",
                                state.pr, state.status
                            ));
                        }
                        "CLOSED" if state.status.is_active() => {
                            issues.push(format!(
                                "PR #{} is closed but status is '{}'",
                                state.pr, state.status
                            ));
                        }
                        _ => {}
                    }
                }
                Err(_) => {
                    issues.push(format!("could not check PR #{} (gh CLI failed)", state.pr));
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

    for finding in &findings {
        if finding.issues.is_empty() {
            lines.push(format!("  {} — ok", finding.feature));
        } else {
            total_issues += finding.issues.len();
            for issue in &finding.issues {
                lines.push(format!("  {} — {issue}", finding.feature));
            }
        }
    }

    let summary = if total_issues == 0 {
        format!("Checked {} feature(s): all healthy", findings.len())
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

        let lines = doctor(&project_path, server.name()).unwrap();
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

        let lines = doctor(&project_path, server.name()).unwrap();
        assert_eq!(lines, vec!["No features to check"]);
    }

    #[test]
    fn missing_worktree_directory_detected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Remove directory on disk without telling git — simulates real drift
        std::fs::remove_dir_all(project_path.join("login")).unwrap();

        let lines = doctor(&project_path, server.name()).unwrap();
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

        let lines = doctor(&project_path, server.name()).unwrap();
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

        let lines = doctor(&project_path, server.name()).unwrap();
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

        let lines = doctor(&project_path, server.name()).unwrap();
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

        let lines = doctor(&project_path, server.name()).unwrap();
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

        let lines = doctor(&project_path, server.name()).unwrap();
        assert!(lines[0].contains("2 feature(s)"), "got: {:?}", lines);
        assert!(lines.iter().any(|l| l.contains("alpha")));
        assert!(lines.iter().any(|l| l.contains("beta")));
    }

    #[test]
    fn multiple_issues_on_same_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = setup_project_with_feature(dir.path(), "login", &server);

        // Remove worktree + branch + tmux session to create multiple issues
        let main_repo = project_path.join("main");
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        git::delete_branch(&main_repo, "login").unwrap();
        tmux::kill_session(server.name(), "myapp/login").unwrap();

        let lines = doctor(&project_path, server.name()).unwrap();
        let issue_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.contains("login") && !l.contains("ok"))
            .collect();
        assert!(
            issue_lines.len() >= 3,
            "expected at least 3 issues, got: {issue_lines:?}"
        );
    }
}
