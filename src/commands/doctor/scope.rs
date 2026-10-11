//! [`diagnose`]: the project-wide checks, the checks every scope shares, and
//! the per-feature worktree, branch, workflow and PR checks.

use std::path::Path;

use super::agents::{agent_issues, legacy_vanilla_agent_issues};
use super::assets::{DefinitionProjections, asset_issues, feature_projection_issues};
use super::config::{harness_config_issues, legacy_vanilla_row_issues};
use super::hooks::hook_issues;
use super::{Depth, Finding, Fix, FixAction, Issue, IssueKind, PROJECT};
use crate::commands::bundled_disable::Disabled;
use crate::commands::feat_delete;
use crate::error::Result;
use crate::harness::Probe;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};
use crate::state::scope::MAIN;
use crate::state::workflow;
use crate::{gh, git, tmux};

/// Run all diagnostic checks without applying any fixes.
///
/// Returns a [`PROJECT`] finding for the project-wide checks (hooks, harness
/// config, asset tiers, the registry's main branch) when it has issues, then
/// one finding per scope — main, then each feature — healthy ones included;
/// callers can filter by inspecting [`Finding::issues`]. Every scope is
/// checked for a paused rebase, legacy agent names and, while active, its
/// session and agents; a feature also for its worktree, branch,
/// initializing, workflow and projection, and PR drift at [`Depth::Full`].
pub fn diagnose(
    project_root: &Path,
    projects_dir: &Path,
    tmux_server: Option<&str>,
    depth: Depth,
) -> Result<Vec<Finding>> {
    let features_dir = paths::features_dir(project_root);
    let config = ProjectConfig::load(&paths::pm_dir(project_root))?;
    let project_name = &config.project.name;
    let main_repo = paths::main_worktree(project_root);
    let main_branch = ProjectEntry::load(projects_dir, project_name)
        .ok()
        .map(|e| e.main_branch);
    let projections = DefinitionProjections::load(project_root)?;

    let mut findings: Vec<Finding> = Vec::new();
    let project_issues = project_issues(
        project_root,
        depth.probe(),
        &projections,
        main_branch.as_deref(),
    )?;
    if !project_issues.is_empty() {
        findings.push(Finding {
            scope: PROJECT.to_string(),
            issues: project_issues,
        });
    }

    let scope = ScopeCheck {
        project_root,
        project_name,
        tmux_server,
    };
    findings.push(Finding {
        scope: MAIN.to_string(),
        issues: scope.issues(MAIN, &main_repo, true)?,
    });

    let worktrees = git::list_worktrees(&main_repo)?;
    for (name, state) in FeatureState::list(&features_dir)? {
        let mut issues = Vec::new();

        let worktree_path = project_root.join(&state.worktree);

        let dir_exists = worktree_path.exists();
        let in_git_worktrees = if let Ok(canonical) = worktree_path.canonicalize() {
            worktrees
                .iter()
                .any(|w| Path::new(w).canonicalize().ok().as_ref() == Some(&canonical))
        } else {
            let wt_str = worktree_path.to_string_lossy();
            worktrees.iter().any(|w| w.ends_with(wt_str.as_ref()))
        };
        let branch_exists = git::branch_exists(&main_repo, &state.branch)?;

        // No directory, no branch, not initializing: one issue instead of
        // redundant individual checks.
        if !dir_exists && !branch_exists && state.status != FeatureStatus::Initializing {
            issues.push(Issue {
                kind: IssueKind::OrphanedState,
                message: "orphaned state file (no worktree, no branch)".to_string(),
                fix: Fix::Auto(FixAction::RemoveState),
            });

            findings.push(Finding {
                scope: name,
                issues,
            });
            continue;
        }

        if !dir_exists {
            // Auto-fix only when the branch exists and git doesn't have a
            // stale worktree registration (which would cause add to fail).
            let fix = if branch_exists && !in_git_worktrees {
                Fix::Auto(FixAction::RecreateWorktree {
                    worktree_path: worktree_path.clone(),
                    branch: state.branch.clone(),
                })
            } else {
                Fix::Skip
            };
            issues.push(Issue {
                kind: IssueKind::WorktreeDirMissing,
                message: "worktree directory missing from disk".to_string(),
                fix,
            });
        }
        if dir_exists && !in_git_worktrees {
            issues.push(Issue {
                kind: IssueKind::DirNotGitWorktree,
                message: "directory exists but not registered as git worktree".to_string(),
                fix: Fix::Skip,
            });
        }
        if !dir_exists && in_git_worktrees {
            issues.push(Issue {
                kind: IssueKind::GitWorktreeNoDir,
                message: "registered as git worktree but directory missing".to_string(),
                fix: Fix::Skip,
            });
        }
        if !branch_exists {
            issues.push(Issue {
                kind: IssueKind::BranchMissing,
                message: format!("branch '{}' not found", state.branch),
                fix: Fix::Skip,
            });
        }

        if state.status == FeatureStatus::Initializing {
            issues.push(Issue {
                kind: IssueKind::StuckInitializing,
                message: "status stuck on 'initializing' (incomplete creation)".to_string(),
                fix: Fix::Auto(FixAction::CleanupInitializing {
                    worktree: state.worktree.clone(),
                    branch: state.branch.clone(),
                    base_scope: main_branch
                        .as_deref()
                        .map(|mb| feat_delete::base_scope(project_root, mb, state.base_branch(mb)))
                        .unwrap_or_else(|| MAIN.to_string()),
                }),
            });
        }

        // Non-fatal: the workflow may have been deleted by accident or not
        // installed on this machine. No auto-fix: a custom workflow can't be
        // regenerated.
        if let Some(wf) = &state.workflow
            && !workflow::exists(project_root, wf)
        {
            let message = if Disabled::load().workflow(wf) {
                format!(
                    "workflow '{wf}' is disabled by `[bundled.disable]` in the global pm config, so its \
                     agents' `pm workflow show` fails (add a project custom or re-enable it)"
                )
            } else {
                format!(
                    "workflow '{wf}' directory missing from .pm/workflows/ \
                     (deleted or not installed on this machine)"
                )
            };
            issues.push(Issue {
                kind: IssueKind::WorkflowDirMissing,
                message,
                fix: Fix::None,
            });
        }

        if dir_exists {
            issues.extend(feature_projection_issues(
                project_root,
                &projections,
                &name,
                &worktree_path,
            )?);
        }

        issues.extend(scope.issues(&name, &worktree_path, state.status.is_active())?);

        if depth == Depth::Full && !state.pr.is_empty() {
            issues.extend(pr_drift_issue(&main_repo, &state));
        }

        findings.push(Finding {
            scope: name,
            issues,
        });
    }

    Ok(findings)
}

/// Checks about the project as a whole rather than one scope.
fn project_issues(
    project_root: &Path,
    probe: Probe,
    projections: &DefinitionProjections,
    main_branch: Option<&str>,
) -> Result<Vec<Issue>> {
    let mut issues = hook_issues(project_root, probe)?;
    issues.extend(harness_config_issues(project_root)?);
    issues.extend(legacy_vanilla_row_issues(project_root)?);
    issues.extend(asset_issues(project_root, projections)?);
    issues.extend(
        main_branch
            .and_then(|recorded| main_branch_issue(&paths::main_worktree(project_root), recorded)),
    );
    Ok(issues)
}

/// What [`ScopeCheck::issues`] needs about the project.
struct ScopeCheck<'a> {
    project_root: &'a Path,
    project_name: &'a str,
    tmux_server: Option<&'a str>,
}

impl ScopeCheck<'_> {
    /// Checks every scope shares: a paused rebase, legacy vanilla agent
    /// names, and, while `active`, its tmux session and agent windows.
    fn issues(&self, scope: &str, worktree: &Path, active: bool) -> Result<Vec<Issue>> {
        let dir_exists = worktree.exists();
        let mut issues = Vec::new();
        if active {
            let session_name = tmux::session_name(self.project_name, scope);
            if !tmux::has_session(self.tmux_server, &session_name)? {
                issues.push(Issue {
                    kind: IssueKind::TmuxSessionMissing,
                    message: format!("tmux session '{session_name}' missing"),
                    fix: if dir_exists {
                        Fix::Auto(FixAction::RecreateTmuxSession {
                            session_name,
                            worktree_path: worktree.to_path_buf(),
                        })
                    } else {
                        Fix::Skip
                    },
                });
            } else {
                issues.extend(agent_issues(
                    self.project_root,
                    scope,
                    &session_name,
                    self.tmux_server,
                )?);
            }
        }
        if dir_exists {
            issues.extend(rebase_issue(worktree));
        }
        issues.extend(legacy_vanilla_agent_issues(self.project_root, scope));
        Ok(issues)
    }
}

/// A linked PR whose GitHub state the feature's status hasn't caught up with.
fn pr_drift_issue(main_repo: &Path, state: &FeatureState) -> Option<Issue> {
    match gh::pr_info(main_repo, &state.pr).map(|i| i.state) {
        Ok(gh_state) => match gh_state.as_str() {
            "MERGED" if state.status != FeatureStatus::Merged => Some(Issue {
                kind: IssueKind::PrMerged,
                message: format!(
                    "PR #{} is merged but status is '{}'",
                    state.pr, state.status
                ),
                fix: Fix::Auto(FixAction::UpdateStatus {
                    new_status: FeatureStatus::Merged,
                }),
            }),
            "CLOSED" if state.status.is_active() => Some(Issue {
                kind: IssueKind::PrClosed,
                message: format!(
                    "PR #{} is closed but status is '{}'",
                    state.pr, state.status
                ),
                fix: Fix::Auto(FixAction::UpdateStatus {
                    new_status: FeatureStatus::Stale,
                }),
            }),
            _ => None,
        },
        Err(_) => Some(Issue {
            kind: IssueKind::PrCheckFailed,
            message: format!("could not check PR #{} (gh CLI failed)", state.pr),
            fix: Fix::None,
        }),
    }
}

/// A rebase paused in `worktree`.
fn rebase_issue(worktree: &Path) -> Option<Issue> {
    git::rebase_in_progress(worktree)
        .unwrap_or(false)
        .then(|| Issue {
            kind: IssueKind::RebaseInProgress,
            message: "rebase in progress (finish with `git rebase --continue` or `--abort`)"
                .to_string(),
            fix: Fix::None,
        })
}

/// Flag a `recorded` registry main branch the repository has no branch for.
fn main_branch_issue(main_repo: &Path, recorded: &str) -> Option<Issue> {
    if git::branch_exists(main_repo, recorded).unwrap_or(true) {
        return None;
    }
    Some(match git::main_branch(main_repo) {
        Ok(branch) => Issue {
            kind: IssueKind::MainBranchMissing,
            message: format!(
                "registry records main branch '{recorded}', which the repo lacks; the repo's main branch is '{branch}'"
            ),
            fix: Fix::Auto(FixAction::RecordMainBranch { branch }),
        },
        Err(_) => Issue {
            kind: IssueKind::MainBranchMissing,
            message: format!(
                "registry records main branch '{recorded}', which the repo lacks, and it has neither origin/HEAD nor a checked-out branch"
            ),
            fix: Fix::Skip,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::doctor;
    use crate::commands::feat_new;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn healthy_feature_reports_ok() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(lines[0].contains("all healthy"), "got: {:?}", lines);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("login") && l.contains("ok"))
        );
    }

    #[test]
    fn a_rebase_paused_in_main_is_reported_for_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        let main = paths::main_worktree(&project_path);
        std::fs::write(main.join("work.txt"), "work").unwrap();
        git::stage_file(&main, "work.txt").unwrap();
        git::commit(&main, "work").unwrap();
        TestServer::pause_rebase(&main, "HEAD~1");

        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        let main_finding = findings.iter().find(|f| f.scope() == "main").unwrap();
        assert!(
            main_finding
                .issues()
                .iter()
                .any(|i| i.kind() == IssueKind::RebaseInProgress)
        );
    }

    #[test]
    fn no_features_reports_main_checked() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert_eq!(
            lines,
            vec!["Checked main and 0 feature(s): all healthy", "  main — ok"]
        );
    }

    #[test]
    fn main_branch_issue_flags_a_recorded_branch_the_repo_lacks() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        let main = paths::main_worktree(&project_path);

        assert!(main_branch_issue(&main, "main").is_none());

        git::rename_branch(&main, "main", "master").unwrap();
        let issue = main_branch_issue(&main, "main").unwrap();
        assert_eq!(issue.kind(), IssueKind::MainBranchMissing);
        assert!(
            matches!(&issue.fix, Fix::Auto(FixAction::RecordMainBranch { branch }) if branch == "master"),
            "{}",
            issue.message()
        );

        git::run_git(&main, &["checkout", "--detach"]).unwrap();
        let issue = main_branch_issue(&main, "main").unwrap();
        assert!(matches!(issue.fix, Fix::Skip), "{}", issue.message());
    }

    #[test]
    fn main_branch_missing_is_read_from_and_fixed_in_the_given_registry() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, project_name) = server.setup_project(dir.path());
        let main = paths::main_worktree(&project_path);

        git::rename_branch(&main, "main", "master").unwrap();

        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        let project = findings.iter().find(|f| f.scope == PROJECT).unwrap();
        assert!(
            project
                .issues
                .iter()
                .any(|i| i.kind() == IssueKind::MainBranchMissing),
            "{:?}",
            project
                .issues
                .iter()
                .map(Issue::message)
                .collect::<Vec<_>>()
        );

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("main branch 'main'")),
            "got: {lines:?}"
        );
        let entry = ProjectEntry::load(&projects_dir, &project_name).unwrap();
        assert_eq!(entry.main_branch, "master");
    }

    #[test]
    fn missing_worktree_directory_detected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Remove directory on disk without telling git — simulates real drift
        std::fs::remove_dir_all(project_path.join("login")).unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
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
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Deregister worktree from git but leave directory on disk
        let main_repo = paths::main_worktree(&project_path);
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        std::fs::create_dir_all(project_path.join("login")).unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
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
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Remove worktree first (branch can't be deleted while checked out), then branch
        let main_repo = paths::main_worktree(&project_path);
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        git::delete_branch(&main_repo, "login").unwrap();
        // Re-create the directory so the only issue is the missing branch
        std::fs::create_dir_all(project_path.join("login")).unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
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
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Kill the feature's tmux session
        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("tmux session '{project_name}/login' missing"))),
            "got: {lines:?}"
        );
    }

    #[test]
    fn stuck_initializing_detected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Manually set the feature status to initializing
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.status = FeatureStatus::Initializing;
        state.save(&features_dir, "login").unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines.iter().any(|l| l.contains("stuck on 'initializing'")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn stuck_initializing_cleanup_returns_to_the_base_feature_scope() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "parent");
        let projects_dir = TestServer::registry_dir(&project_path);
        feat_new::feat_new(&feat_new::FeatNewParams {
            project_root: &project_path,
            projects_dir: &projects_dir,
            name: "child",
            name_override: None,
            context: None,
            base: Some("parent"),
            workflow: None,
            tmux_server: server.name(),
        })
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "child").unwrap();
        state.status = FeatureStatus::Initializing;
        state.save(&features_dir, "child").unwrap();

        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        let child = findings.iter().find(|f| f.scope == "child").unwrap();
        let base_scope = child
            .issues
            .iter()
            .find_map(|i| match &i.fix {
                Fix::Auto(FixAction::CleanupInitializing { base_scope, .. }) => Some(base_scope),
                _ => None,
            })
            .expect("stuck-initializing issue");
        assert_eq!(base_scope, "parent");
    }

    #[test]
    fn missing_workflow_directory_detected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Point the feature at a workflow whose directory does not exist.
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.workflow = Some("ghost-workflow".to_string());
        state.save(&features_dir, "login").unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines.iter().any(|l| l.contains("login")
                && l.contains("workflow 'ghost-workflow'")
                && l.contains("missing")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn present_workflow_directory_not_flagged() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Create a workflow directory and point the feature at it.
        let wf_dir = paths::workflows_dir(&project_path).join("real-workflow");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(wf_dir.join("config.toml"), "description = \"x\"\n").unwrap();
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.workflow = Some("real-workflow".to_string());
        state.save(&features_dir, "login").unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            !lines.iter().any(|l| l.contains("workflow")),
            "got: {lines:?}"
        );
    }

    // Check 7 (PR state drift) is not unit-tested because it requires `gh` CLI
    // authenticated against a real GitHub remote. The logic is exercised via the
    // gh::pr_info wrapper; integration testing would need a mock or real repo.

    #[test]
    fn multiple_features_all_checked() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "alpha",
            server.name(),
        ))
        .unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "beta",
            server.name(),
        ))
        .unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines[0].contains("main and 2 feature(s)"),
            "got: {:?}",
            lines
        );
        assert!(lines.iter().any(|l| l.contains("alpha")));
        assert!(lines.iter().any(|l| l.contains("beta")));
    }

    #[test]
    fn multiple_issues_on_same_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Remove worktree + branch + tmux session — fully orphaned
        let main_repo = paths::main_worktree(&project_path);
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        git::delete_branch(&main_repo, "login").unwrap();
        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
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
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Remove only the worktree directory — branch still exists, so not orphaned
        std::fs::remove_dir_all(project_path.join("login")).unwrap();
        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        let issue_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.contains("login") && !l.contains("ok"))
            .collect();
        assert!(
            issue_lines.len() >= 2,
            "expected at least 2 issues, got: {issue_lines:?}"
        );
    }
}
