use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::commands::attention::{self, AgentState};
use crate::commands::bundled_disable::Disabled;
use crate::commands::feat_delete::{self, CleanupParams};
use crate::commands::harness_check::{self, Problem, ProblemKind};
use crate::commands::running_agents::Windows;
use crate::commands::{agent_spawn, hooks_install, launch_stamp, skills, vanilla_rename};
use crate::error::Result;
use crate::harness::{ConfigIssueKind, Harness, Probe};
use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::{
    AgentsConfig, GlobalConfig, ProjectConfig, ProjectEntry, WILDCARD_AGENT, harness_config,
    resolve_agent_settings,
};
use crate::state::workflow::{self, LEGACY_VANILLA_AGENT, VANILLA_AGENT};
use crate::{gh, git, tmux};

/// Categorisation of an issue for callers that want to filter findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    /// State file present but worktree directory and branch are gone.
    OrphanedState,
    /// Worktree directory missing on disk.
    WorktreeDirMissing,
    /// Directory exists but git doesn't know about it as a worktree.
    DirNotGitWorktree,
    /// Git lists the worktree but the directory doesn't exist on disk.
    GitWorktreeNoDir,
    /// Feature branch missing from git.
    BranchMissing,
    /// Tmux session for an active scope is missing.
    TmuxSessionMissing,
    /// Agent registered as active but its tmux window is gone.
    AgentWindowMissing,
    /// An active agent's window is up but no session id has been recorded
    /// for it since its spawn.
    AgentSessionNotStarted,
    /// An active agent's window is up but its harness exited to the shell.
    AgentHarnessExited,
    /// Feature status stuck on `initializing`.
    StuckInitializing,
    /// Feature references a workflow whose directory is missing.
    WorkflowDirMissing,
    /// PR merged upstream but local status hasn't caught up.
    PrMerged,
    /// PR closed upstream but local status still active.
    PrClosed,
    /// `gh` lookup failed for a linked PR.
    PrCheckFailed,
    /// pm hooks not installed in the harness's user-level settings file.
    HooksNotInstalled,
    /// pm hook entries an earlier release wrote into a project-level
    /// settings file are still there.
    StaleProjectHooks,
    /// A canonical agent definition has no projected copy for a harness in
    /// use, so validation passes but the harness can't launch it.
    AssetNotProjected,
    /// A bundled asset is missing from the global tier.
    GlobalStoreMissing,
    /// Pre-migration bundled copies in the project shadow the global tier.
    StaleBundledCopies,
    /// The global config's `[bundled.disable]` table disables bundled items.
    BundledDisabled,
    /// `[bundled.disable]` names something pm doesn't bundle.
    BundledUnknown,
    /// A bundled item `[bundled.disable]` lists is still in the global tier or a
    /// harness's projection of it.
    DisabledStillInstalled,
    /// Something pm or a workflow needs is disabled and no custom provides it.
    BundledDisabledDangling,
    /// A project override whose content equals the bundled asset it shadows.
    RedundantOverride,
    /// A project custom skill the harness resolves its global namesake over.
    SkillShadowedByGlobal,
    /// An active agent is named `claude`, the removed vanilla alias: it runs
    /// until its window dies, then restart/heal fail to resolve a definition.
    /// Also an agent registered as `default` in a project `pm upgrade` has not
    /// yet migrated to `plain` (`commands::vanilla_rename`).
    LegacyVanillaAgentName,
    /// An `[agents.*]` row keyed `default`, which names no definition: it
    /// reads as a catch-all but matches only an agent defined as `default`.
    LegacyVanillaConfigRow,
    /// A harness's hooks file has an entry in a shape the harness silently
    /// registers nothing for.
    HooksMalformed,
    /// The harness has not recorded trust for a pm hook, so it silently does
    /// not run it.
    HookUntrusted,
    /// A worktree is not trusted by the harness, so it stops at an
    /// interactive prompt on launch.
    WorktreeUntrusted,
    /// The registry's `main_branch` names a branch the repository does not
    /// have, so merge-safety checks compare against nothing.
    MainBranchMissing,
    /// A harness in use can't run agents as installed.
    HarnessUnusable,
    /// An agent's emulated never-idle loop stopped itself, so the agent no
    /// longer wakes for messages.
    LoopStopped,
    /// An agent's window is up, but its emulated never-idle loop has not
    /// loaded since its spawn, so it never wakes for messages.
    LoopNotLoaded,
    /// An agent's last turn failed, and its loop is still retrying.
    TurnFailed,
    /// An agent runs on a harness that refuses to spawn it without an
    /// `[agents.models]` row, and has none.
    AgentModelMissing,
    /// An agent has a model or permission row its harness refuses to spawn
    /// with.
    AgentRowInvalid,
    /// An agent's model row has a consequence worth knowing (a model its
    /// provider does not declare); the spawn goes ahead.
    AgentRowRemark,
    /// A harness in use reports a problem with its `[harness.<name>]`
    /// settings.
    HarnessConfigInvalid,
    /// A running agent would launch differently now than it did: it runs
    /// on an outdated definition, prompt, config row or never-idle loop.
    AgentLaunchStale,
    /// A provider's key variable is unset in pm's environment. Advisory:
    /// the agent's own environment may set it.
    ProviderKeyUnset,
    /// A provider that a definition runs on, or that the user configured
    /// outside pm, is out of reach of pm's agents.
    ProviderUnreachable,
    /// A feature's or main's worktree has a rebase paused, so its branch
    /// does not yet hold the rebased commits and `pm feat merge` refuses it.
    RebaseInProgress,
}

/// A single issue detected for a feature.
pub struct Issue {
    kind: IssueKind,
    message: String,
    fix: Fix,
}

impl Issue {
    /// Category of the issue, for filtering.
    pub fn kind(&self) -> IssueKind {
        self.kind
    }

    /// Human-readable description of the issue.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether `pm doctor --fix` resolves the issue.
    pub fn auto_fixable(&self) -> bool {
        matches!(self.fix, Fix::Auto(_))
    }
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
    CleanupInitializing {
        worktree: String,
        branch: String,
        base_scope: String,
    },
    /// Recreate a missing tmux session.
    RecreateTmuxSession {
        session_name: String,
        worktree_path: PathBuf,
    },
    /// Update feature status to match GH PR state.
    UpdateStatus { new_status: FeatureStatus },
    /// Install the pm hooks at the user level and strip them from project files.
    InstallStopHook,
    /// (Re)install the bundled assets into the global tier.
    InstallGlobalAssets,
    /// Recreate a missing worktree from its branch.
    RecreateWorktree {
        worktree_path: PathBuf,
        branch: String,
    },
    /// Clear stale active flag and respawn a dead agent.
    RespawnAgent { agent_name: String },
    /// Record directory trust for a worktree with the harness.
    TrustWorktree { harness: Harness, path: PathBuf },
    /// Rewrite the registry entry's `main_branch`.
    RecordMainBranch { branch: String },
    /// Point registered `default` agents at `plain` (`vanilla_rename`).
    MigrateVanillaAgents,
    /// `pm harness pull` the feature.
    PullFeatureAssets,
}

/// Diagnostic finding for a single scope (a feature or `main`).
pub struct Finding {
    feature: String,
    issues: Vec<Issue>,
}

impl Finding {
    /// Name of the scope this finding pertains to (`main` or a feature name).
    pub fn feature(&self) -> &str {
        &self.feature
    }

    /// Issues detected for this scope.
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }
}

/// How much a [`diagnose`] may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// `pm doctor`: PR drift checks (one `gh` call per feature with a PR)
    /// and fresh harness probes.
    Full,
    /// Latency-sensitive callers (`pm status`, `pm open`'s pre-recreate
    /// warning): no `gh` calls, and harness probes from cache.
    Quick,
}

impl Depth {
    fn probe(self) -> Probe {
        match self {
            Depth::Full => Probe::Fresh,
            Depth::Quick => Probe::Cached,
        }
    }
}

/// Run all diagnostic checks without applying any fixes.
///
/// For each feature, checks:
/// 1. Worktree directory exists on disk
/// 2. Git worktree list includes it
/// 3. Branch exists locally
/// 4. Tmux session exists
/// 5. Status stuck on "initializing"
/// 6. Referenced workflow directory exists
/// 7. If PR linked and `depth` is [`Depth::Full`], check GH status drift
///
/// Also runs main-scope checks (Stop hook installed, main session present, main
/// agent windows alive) when there is at least one feature, mirroring the
/// existing `doctor` behaviour.
///
/// Returns one [`Finding`] per scope that has issues (or per scope, including
/// healthy ones — callers can filter by inspecting [`Finding::issues`]).
pub fn diagnose(
    project_root: &Path,
    projects_dir: &Path,
    tmux_server: Option<&str>,
    depth: Depth,
) -> Result<Vec<Finding>> {
    let probe = depth.probe();
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let features = FeatureState::list(&features_dir)?;
    let main_repo = paths::main_worktree(project_root);
    let mut findings: Vec<Finding> = Vec::new();

    // Main-scope checks: stop hook installed, main tmux session present.
    let main_session = tmux::session_name(project_name, "main");
    let mut main_issues: Vec<Issue> = Vec::new();
    main_issues.extend(hook_issues(project_root, probe)?);
    for path in hooks_install::stale_project_files(project_root)? {
        main_issues.push(Issue {
            kind: IssueKind::StaleProjectHooks,
            message: format!(
                "pm hooks still in {} (run `pm harness hooks install`)",
                path.strip_prefix(project_root).unwrap_or(&path).display()
            ),
            fix: Fix::Auto(FixAction::InstallStopHook),
        });
    }
    main_issues.extend(harness_config_issues(project_root)?);
    main_issues.extend(legacy_vanilla_row_issues(project_root)?);
    let projections = DefinitionProjections::load(project_root)?;
    main_issues.extend(asset_issues(project_root, &projections)?);
    main_issues.extend(rebase_issue(&main_repo));
    main_issues.extend(legacy_vanilla_agent_issues(project_root, "main"));
    let main_branch = ProjectEntry::load(projects_dir, project_name)
        .ok()
        .map(|e| e.main_branch);
    main_issues.extend(
        main_branch
            .as_deref()
            .and_then(|recorded| main_branch_issue(&main_repo, recorded)),
    );
    if !tmux::has_session(tmux_server, &main_session)? {
        main_issues.push(Issue {
            kind: IssueKind::TmuxSessionMissing,
            message: format!("tmux session '{main_session}' missing (run `pm open` to fix)"),
            fix: Fix::Auto(FixAction::RecreateTmuxSession {
                session_name: main_session.clone(),
                worktree_path: main_repo.clone(),
            }),
        });
    } else {
        main_issues.extend(agent_issues(
            project_root,
            "main",
            &main_session,
            tmux_server,
        )?);
    }
    if !main_issues.is_empty() {
        findings.push(Finding {
            feature: "main".to_string(),
            issues: main_issues,
        });
    }

    let worktrees = git::list_worktrees(&main_repo)?;
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
                kind: IssueKind::OrphanedState,
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

        // Check 4: tmux session exists (only for active features)
        if state.status.is_active() {
            let session_name = tmux::session_name(project_name, name);
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
                    kind: IssueKind::TmuxSessionMissing,
                    message: format!("tmux session '{session_name}' missing"),
                    fix: fix_action,
                });
            } else {
                // Check 4b: agents within the existing session
                issues.extend(agent_issues(
                    project_root,
                    name,
                    &session_name,
                    tmux_server,
                )?);
            }
        }

        // Check 5: stuck on initializing
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
                        .unwrap_or_else(|| "main".to_string()),
                }),
            });
        }

        // Check 6: referenced workflow directory exists. Non-fatal warning —
        // the workflow may have been deleted by accident or not installed on
        // this machine. No auto-fix: a custom workflow can't be regenerated.
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
                name,
                &worktree_path,
            )?);
        }

        if dir_exists {
            issues.extend(rebase_issue(&worktree_path));
        }

        issues.extend(legacy_vanilla_agent_issues(project_root, name));

        // Check 7: PR status drift.
        if depth == Depth::Full && !state.pr.is_empty() {
            match gh::pr_info(&main_repo, &state.pr).map(|i| i.state) {
                Ok(gh_state) => match gh_state.as_str() {
                    "MERGED" if state.status != FeatureStatus::Merged => {
                        issues.push(Issue {
                            kind: IssueKind::PrMerged,
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
                            kind: IssueKind::PrClosed,
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
                },
                Err(_) => {
                    issues.push(Issue {
                        kind: IssueKind::PrCheckFailed,
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

    Ok(findings)
}

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

/// Warn when the global `config.toml` exists but doesn't parse. Every reader
/// falls back to defaults on a parse error so that a bad file can never block a
/// spawn, which leaves `pm doctor` as the only place a user hand-editing it can
/// learn that `max_features` and all their per-agent models and permission
/// modes are being ignored.
fn global_config_warning() -> Option<String> {
    global_config_warning_in(&paths::global_config_dir().ok()?)
}

fn global_config_warning_in(config_dir: &Path) -> Option<String> {
    let err = GlobalConfig::load(config_dir).err()?;
    let path = config_dir.join("config.toml").display().to_string();
    Some(format!(
        "global config — {path} could not be read ({err}); max_features, [agents.models] and [agents.permissions] from it are all being ignored"
    ))
}

/// `pm serve` ([`serve_status`](crate::commands::serve_status)). Tailscale
/// is asked only at full depth.
fn serve_warnings(depth: Depth) -> Vec<String> {
    let (Ok(home), Ok(config_dir), Ok(exe)) = (
        paths::home_dir(),
        paths::global_config_dir(),
        std::env::current_exe(),
    ) else {
        return Vec::new();
    };
    let port = crate::commands::serve::configured_port(&config_dir);
    crate::commands::serve_status::Facts::read(&home, &config_dir, port).warnings(
        &exe,
        crate::version::VERSION,
        |port| (depth == Depth::Full).then(|| crate::tailscale::check(port)),
    )
}

/// Warn about each registry entry that can't be read; all-project commands
/// skip it.
fn registry_warnings(projects_dir: &Path) -> Result<Vec<String>> {
    Ok(ProjectEntry::scan(projects_dir)?
        .malformed
        .iter()
        .map(|bad| {
            format!(
                "registry — {} could not be read ({}); all-project commands skip project '{}'",
                bad.path.display(),
                bad.error,
                bad.name
            )
        })
        .collect())
}

/// Warn when the shared agent baseline is installed for this project but a
/// harness in use can't deliver pm's composed prompt — how the baseline
/// reaches an agent at spawn time. Nothing when the baseline isn't installed
/// (nothing to apply) or a binary can't be probed.
fn baseline_capability_warnings(project_root: &Path, probe: Probe) -> Result<Vec<String>> {
    if !crate::commands::skills::baseline_path(project_root).exists() {
        return Ok(Vec::new());
    }
    let config = harness_config(Some(project_root));
    Ok(skills::harnesses_in_use(project_root)?
        .into_iter()
        // An unusable harness is reported as a finding of its own.
        .filter(|h| h.unusable_reason(&config, probe).is_none())
        .filter(|h| h.supports_prompt_delivery(&config, probe) == Some(false))
        .map(|h| format!("baseline — {}", prompt_delivery_unsupported(h)))
        .collect())
}

fn prompt_delivery_unsupported(harness: Harness) -> String {
    format!(
        "{harness} does not support {}; the shared agent baseline \
         (~/.agents/pm-baseline.md) will NOT be applied to spawned agents. Check your \
         {harness} version.",
        harness.prompt_mechanism()
    )
}

/// One line for `pm harness probe`: whether the installed binary supports
/// the capabilities pm relies on.
pub fn probe_line(harness: Harness, project_root: Option<&Path>) -> String {
    let mechanism = harness.prompt_mechanism();
    match harness.supports_prompt_delivery(&harness_config(project_root), Probe::Fresh) {
        Some(true) => format!("{harness}: {mechanism} supported — the shared baseline is applied"),
        Some(false) => format!(
            "{harness}: does not support {mechanism} — the shared agent baseline will not be \
             applied to spawned agents"
        ),
        None => format!("{harness}: binary not found (or probing it failed); nothing to probe"),
    }
}

/// Main-scope findings about each harness in use's hooks: pm's entries
/// missing from its user-level file, entries in a shape it would ignore,
/// pm hooks it has not been told to trust, and worktrees whose agents run
/// on it that it would stop at a trust prompt for. The trust findings exist
/// because codex fails silently on both counts. Only harnesses in use, although the install
/// writes every supported harness's file: a trust finding for a harness
/// none of this project's agents run on would be noise.
fn hook_issues(project_root: &Path, probe: Probe) -> Result<Vec<Issue>> {
    hook_issues_in(project_root, &paths::home_dir()?, probe)
}

/// [`hook_issues`] against an explicit `home`.
fn hook_issues_in(project_root: &Path, home: &Path, probe: Probe) -> Result<Vec<Issue>> {
    let worktree_harnesses = worktree_harnesses(project_root)?;
    let (project, global) = agents_configs(project_root)?;
    let config = harness_config(Some(project_root));
    let mut issues = Vec::new();
    for harness in skills::harnesses_in_use(project_root)? {
        for Problem { kind, message } in
            harness_check::harness_problems(harness, &config, home, probe)?
        {
            // The default harness is in use whether or not an agent is on it.
            if kind == ProblemKind::Unusable
                && !harness_check::has_agents(harness, &project, &global)
            {
                continue;
            }
            issues.push(match kind {
                ProblemKind::Unusable => Issue {
                    kind: IssueKind::HarnessUnusable,
                    message: format!("agents configured for {harness} cannot run: {message}"),
                    fix: Fix::None,
                },
                ProblemKind::LoopNotInstalled | ProblemKind::StatusHooksMissing => Issue {
                    kind: IssueKind::HooksNotInstalled,
                    message,
                    fix: Fix::Auto(FixAction::InstallStopHook),
                },
                ProblemKind::HooksMalformed => Issue {
                    kind: IssueKind::HooksMalformed,
                    message,
                    fix: Fix::None,
                },
                ProblemKind::HookUntrusted
                | ProblemKind::ResetHookUntrusted
                | ProblemKind::StatusHookUntrusted => Issue {
                    kind: IssueKind::HookUntrusted,
                    message,
                    fix: Fix::None,
                },
            });
        }
        for (wt, needed) in &worktree_harnesses {
            if needed.contains(&harness) && !harness.worktree_trusted(home, wt) {
                issues.push(Issue {
                    kind: IssueKind::WorktreeUntrusted,
                    message: format!(
                        "{} is not trusted by {harness}; it will stop at a trust prompt on launch",
                        wt.strip_prefix(project_root).unwrap_or(wt).display()
                    ),
                    fix: Fix::Auto(FixAction::TrustWorktree {
                        harness,
                        path: wt.clone(),
                    }),
                });
            }
        }
    }
    Ok(issues)
}

/// Main-scope findings about what each harness in use is configured with:
/// agents it will refuse for want of a model row or remarks on at spawn,
/// and whatever the harness itself reports about its `[harness.<name>]`
/// settings.
fn harness_config_issues(project_root: &Path) -> Result<Vec<Issue>> {
    let (project, global) = agents_configs(project_root)?;
    let mut definitions: Vec<String> = project
        .harness
        .keys()
        .chain(global.harness.keys())
        .filter(|key| *key != WILDCARD_AGENT)
        .cloned()
        .collect();
    for (_, launched) in worktree_definitions(project_root)? {
        definitions.extend(launched);
    }
    definitions.sort();
    definitions.dedup();

    let config = harness_config(Some(project_root));
    let mut issues = Vec::new();
    let mut rows: Vec<(Harness, String)> = Vec::new();
    // The catch-all row reaches agents no other row names.
    if let Ok(settings) = resolve_agent_settings(&project, &global, WILDCARD_AGENT)
        && let Some(model) = settings.model
    {
        rows.push((settings.harness, model));
    }
    for definition in definitions {
        let Ok(settings) = resolve_agent_settings(&project, &global, &definition) else {
            continue;
        };
        if let Some(model) = &settings.model {
            rows.push((settings.harness, model.clone()));
        }
        for note in settings
            .harness
            .row_notes(&config, settings.model.as_deref())
        {
            issues.push(Issue {
                kind: IssueKind::AgentRowRemark,
                message: format!("agent '{definition}': {note}"),
                fix: Fix::None,
            });
        }
        if let Some(dropped) = harness_check::missing_model_row(&settings) {
            issues.push(Issue {
                kind: IssueKind::AgentModelMissing,
                message: format!(
                    "agent '{definition}' runs on {} and has no [agents.models] row, so it \
                     will not spawn{dropped}",
                    settings.harness
                ),
                fix: Fix::None,
            });
        }
        for issue in harness_check::row_issues(&settings) {
            issues.push(Issue {
                kind: IssueKind::AgentRowInvalid,
                message: format!("agent '{definition}' will not spawn: {issue}"),
                fix: Fix::None,
            });
        }
    }

    let main = paths::main_worktree(project_root);
    for harness in skills::harnesses_in_use(project_root)? {
        let rows: Vec<String> = rows
            .iter()
            .filter(|(on, _)| *on == harness)
            .map(|(_, row)| row.clone())
            .collect();
        for issue in harness.config_issues(&config, &main, &rows) {
            issues.push(Issue {
                kind: match issue.kind {
                    ConfigIssueKind::Invalid => IssueKind::HarnessConfigInvalid,
                    ConfigIssueKind::KeyUnset => IssueKind::ProviderKeyUnset,
                    ConfigIssueKind::ProviderUnreachable => IssueKind::ProviderUnreachable,
                },
                message: issue.message,
                fix: Fix::None,
            });
        }
    }
    Ok(issues)
}

/// The `[agents.*]` tables of the two tiers.
fn agents_configs(project_root: &Path) -> Result<(AgentsConfig, AgentsConfig)> {
    let project = match ProjectConfig::load(&paths::pm_dir(project_root)) {
        Ok(config) => config.agents,
        Err(crate::error::PmError::NotInProject) => Default::default(),
        Err(e) => return Err(e),
    };
    Ok((project, GlobalConfig::load_or_default().agents))
}

/// Each worktree on disk with the definitions launched in it: those of its
/// registered agents plus, for a feature, its workflow team.
fn worktree_definitions(project_root: &Path) -> Result<Vec<(PathBuf, Vec<String>)>> {
    let agents_dir = paths::agents_dir(project_root);
    let features_dir = paths::features_dir(project_root);
    let mut out = Vec::new();
    for (scope, wt) in skills::scoped_worktrees_on_disk(project_root)? {
        let registry = AgentRegistry::load(&agents_dir, &scope)?;
        let mut definitions: Vec<String> = registry
            .agents
            .iter()
            .map(|(key, entry)| entry.effective_definition(key).to_string())
            .collect();
        if let Ok(feature) = FeatureState::load(&features_dir, &scope)
            && let Some(name) = &feature.workflow
            && let Ok(def) = workflow::WorkflowDef::load(project_root, name)
        {
            definitions.extend(def.effective_team().iter().cloned());
        }
        out.push((wt, definitions));
    }
    Ok(out)
}

/// Each worktree on disk with the harnesses its agents launch on — the set
/// a spawn there would need the worktree trusted by.
fn worktree_harnesses(project_root: &Path) -> Result<Vec<(PathBuf, Vec<Harness>)>> {
    let (project, global) = agents_configs(project_root)?;
    let mut out = Vec::new();
    for (wt, definitions) in worktree_definitions(project_root)? {
        let mut harnesses = Vec::new();
        for def in definitions {
            if let Ok(h) = agent_spawn::configured_harness(&def, &project, &global)
                && !harnesses.contains(&h)
            {
                harnesses.push(h);
            }
        }
        out.push((wt, harnesses));
    }
    Ok(out)
}

/// Main-scope findings about the two asset tiers: what the global tier is
/// missing, pre-migration copies still shadowing it, definitions a harness
/// can't see, and project skills its global namesake shadows.
fn asset_issues(project_root: &Path, projections: &DefinitionProjections) -> Result<Vec<Issue>> {
    let mut issues = Vec::new();

    let missing = skills::global_store_missing()?;
    if !missing.is_empty() {
        issues.push(Issue {
            kind: IssueKind::GlobalStoreMissing,
            message: format!(
                "not installed in the global asset store: {} (run `pm upgrade`)",
                missing.join(", ")
            ),
            fix: Fix::Auto(FixAction::InstallGlobalAssets),
        });
    }

    issues.extend(bundled_issues(
        project_root,
        &workflow::global_dir()?,
        paths::home_dir().ok().as_deref(),
        &Disabled::load(),
    )?);
    let still_installed = skills::disabled_still_installed()?;
    if !still_installed.is_empty() {
        issues.push(Issue {
            kind: IssueKind::DisabledStillInstalled,
            message: format!(
                "disabled by `[bundled.disable]` but still installed: {} (run `pm upgrade`)",
                still_installed.join(", ")
            ),
            fix: Fix::Auto(FixAction::InstallGlobalAssets),
        });
    }

    if !skills::is_migrated(project_root) {
        let stale = skills::stale_bundled_copies(project_root)?;
        if !stale.is_empty() {
            issues.push(Issue {
                kind: IssueKind::StaleBundledCopies,
                message: format!(
                    "{} pre-migration bundled copies shadow the global store (run `pm upgrade`)",
                    stale.len()
                ),
                fix: Fix::None,
            });
        }
    } else {
        let redundant = skills::redundant_overrides(project_root);
        if !redundant.is_empty() {
            issues.push(Issue {
                kind: IssueKind::RedundantOverride,
                message: format!(
                    "project overrides identical to the bundled asset (delete to follow the \
                     global store): {}",
                    redundant.join(", ")
                ),
                fix: Fix::None,
            });
        }
    }

    for (name, harness) in unprojected_definitions(projections)? {
        issues.push(Issue {
            kind: IssueKind::AssetNotProjected,
            message: format!(
                "canonical agent '{name}' not projected for {harness} (run `pm upgrade`)"
            ),
            fix: Fix::Skip,
        });
    }

    for (name, harness) in skills::shadowed_project_skills(project_root)? {
        issues.push(Issue {
            kind: IssueKind::SkillShadowedByGlobal,
            message: format!(
                "project skill '{name}' never applies: {harness} resolves the personal skill of \
                 that name over it — rename the custom"
            ),
            fix: Fix::None,
        });
    }

    Ok(issues)
}

/// Findings about the global config's `[bundled.disable]` table: what it disables,
/// names in it pm doesn't bundle, and references to a disabled name that no
/// custom resolves.
fn bundled_issues(
    project_root: &Path,
    global_dir: &Path,
    home: Option<&Path>,
    disabled: &Disabled,
) -> Result<Vec<Issue>> {
    let mut issues = Vec::new();
    let items = disabled.items();
    if !items.is_empty() {
        issues.push(Issue {
            kind: IssueKind::BundledDisabled,
            message: format!("disabled by `[bundled.disable]`: {}", items.join(", ")),
            fix: Fix::None,
        });
    }
    let unknown = disabled.unknown();
    if !unknown.is_empty() {
        issues.push(Issue {
            kind: IssueKind::BundledUnknown,
            message: format!(
                "`[bundled.disable]` names nothing pm bundles: {}",
                unknown.join(", ")
            ),
            fix: Fix::None,
        });
    }
    for reference in disabled.dangling(project_root, global_dir, home)? {
        issues.push(Issue {
            kind: IssueKind::BundledDisabledDangling,
            message: format!(
                "{reference}, which `[bundled.disable]` lists (add a project custom or re-enable it)"
            ),
            fix: Fix::None,
        });
    }
    Ok(issues)
}

/// Which harnesses need projected definitions, and main's canonical
/// (project-tier) definitions each lacks a projected copy of in main.
struct DefinitionProjections {
    harnesses: Vec<Harness>,
    missing_in_main: Vec<(String, Harness)>,
}

impl DefinitionProjections {
    fn load(project_root: &Path) -> Result<Self> {
        let harnesses: Vec<Harness> = skills::harnesses_in_use(project_root)?
            .into_iter()
            .filter(|h| h.projects_definitions())
            .collect();
        let main = paths::main_worktree(project_root);
        let missing_in_main = unprojected_project_definitions(&main, &main, &harnesses)?;
        Ok(Self {
            harnesses,
            missing_in_main,
        })
    }
}

/// Agent definitions with no projected copy in a harness's own definition
/// dir, in either tier. Such a definition passes `WorkflowDef::validate`
/// but the harness can't find it at launch.
fn unprojected_definitions(projections: &DefinitionProjections) -> Result<Vec<(String, Harness)>> {
    let mut out = skills::unprojected_global_definitions(&projections.harnesses)?;
    out.extend(projections.missing_in_main.iter().cloned());
    Ok(out)
}

/// Main's canonical (project-tier) definitions with no projected copy in
/// `worktree`'s harness dirs.
fn unprojected_project_definitions(
    main: &Path,
    worktree: &Path,
    harnesses: &[Harness],
) -> Result<Vec<(String, Harness)>> {
    let canonical = main.join(skills::CANONICAL_DIR).join("agents");
    let mut out = Vec::new();
    for file in skills::definition_files(&canonical)? {
        for harness in harnesses {
            let projected = worktree
                .join(harness.config_dir())
                .join("agents")
                .join(&file);
            if !projected.exists() {
                out.push((file.trim_end_matches(".md").to_string(), *harness));
            }
        }
    }
    Ok(out)
}

/// Project-tier definitions a feature worktree lacks a projected copy of.
/// `pm upgrade` projects into main only; the harness, started in the
/// feature, never sees main's copy.
fn feature_projection_issues(
    project_root: &Path,
    projections: &DefinitionProjections,
    feature: &str,
    worktree: &Path,
) -> Result<Vec<Issue>> {
    let main = paths::main_worktree(project_root);
    let pull = format!("`pm harness pull {feature}`");
    Ok(
        unprojected_project_definitions(&main, worktree, &projections.harnesses)?
            .into_iter()
            .map(|(name, harness)| {
                let main_lacks = projections
                    .missing_in_main
                    .contains(&(name.clone(), harness));
                let (remedy, fix) = if main_lacks {
                    (format!("run `pm upgrade`, then {pull}"), Fix::None)
                } else {
                    (
                        format!("run {pull}"),
                        Fix::Auto(FixAction::PullFeatureAssets),
                    )
                };
                Issue {
                    kind: IssueKind::AssetNotProjected,
                    message: format!(
                        "canonical agent '{name}' not projected into this worktree for \
                         {harness} ({remedy})"
                    ),
                    fix,
                }
            })
            .collect(),
    )
}

/// How long after its spawn an agent may go without a recorded session id,
/// or a loaded never-idle loop, before that is reported.
pub(crate) const START_GRACE: Duration = Duration::from_secs(60);

/// Findings about `scope`'s active agents, given its tmux session exists.
fn agent_issues(
    project_root: &Path,
    scope: &str,
    session_name: &str,
    tmux_server: Option<&str>,
) -> Result<Vec<Issue>> {
    let agents_dir = paths::agents_dir(project_root);
    let Ok(registry) = AgentRegistry::load(&agents_dir, scope) else {
        return Ok(Vec::new());
    };
    // An entry with no spawn time is at least as old as the file holding it.
    let registry_written = AgentRegistry::modified(&agents_dir, scope);
    let past_grace = |entry: &AgentEntry| {
        entry
            .spawned_at
            .map(SystemTime::from)
            .or(registry_written)
            .and_then(|since| since.elapsed().ok())
            .is_some_and(|age| age > START_GRACE)
    };
    let windows = Windows::read(tmux_server)?;
    // What the attention snapshot reads each agent as; unreadable, none.
    let states = attention::scope_agents_in(project_root, scope, &windows).unwrap_or_default();
    let config = ProjectConfig::load(&paths::pm_dir(project_root)).ok();
    let global = GlobalConfig::load_or_default();
    let mut issues = Vec::new();
    for (agent_name, entry) in &registry.agents {
        if entry.agent_type != AgentType::Agent || !entry.active {
            continue;
        }
        if windows.find(session_name, &entry.window_name).is_none() {
            issues.push(Issue {
                kind: IssueKind::AgentWindowMissing,
                message: format!("agent '{agent_name}' registered as active but window missing"),
                fix: Fix::Auto(FixAction::RespawnAgent {
                    agent_name: agent_name.clone(),
                }),
            });
            continue;
        }
        if states
            .iter()
            .any(|a| a.name == *agent_name && a.state == AgentState::Dead)
        {
            issues.push(Issue {
                kind: IssueKind::AgentHarnessExited,
                message: format!(
                    "agent '{agent_name}' has its window open but its {} harness exited \
                     (its window shows why; fix that, then `pm agent restart {agent_name} \
                     --scope {scope}`)",
                    entry.harness
                ),
                fix: Fix::None,
            });
            continue;
        }
        issues.extend(loop_issue(project_root, scope, agent_name, entry));
        if let Some(config) = &config
            && launch_stamp::is_stale(project_root, scope, agent_name, entry, config, &global)
                .unwrap_or(false)
        {
            issues.push(Issue {
                kind: IssueKind::AgentLaunchStale,
                message: format!(
                    "agent '{agent_name}' runs on what it was launched with, which has changed \
                     since (its definition, the baseline, a notice board, a config row, or pm's \
                     hooks); `pm agent restart {agent_name} --scope {scope}` relaunches it"
                ),
                fix: Fix::None,
            });
        }
        if past_grace(entry) {
            if entry.session_id.is_empty() {
                issues.push(Issue {
                    kind: IssueKind::AgentSessionNotStarted,
                    message: format!(
                        "agent '{agent_name}' is running but its {} session has recorded no \
                         session id (run `pm agent restart {agent_name} --scope {scope}`)",
                        entry.harness
                    ),
                    fix: Fix::None,
                });
            } else if entry.harness.loop_loaded(project_root, scope, agent_name) == Some(false) {
                issues.push(Issue {
                    kind: IssueKind::LoopNotLoaded,
                    message: format!(
                        "agent '{agent_name}' is running but {} has not loaded pm's never-idle \
                         plugin, so it never wakes for messages (`/plugins` in its window shows \
                         why; then `pm agent restart {agent_name} --scope {scope}`)",
                        entry.harness
                    ),
                    fix: Fix::None,
                });
            }
        }
    }
    Ok(issues)
}

/// Whether `agent`'s never-idle loop stopped itself, or its last turn
/// failed while the loop still retries. Not fixable here: a restart re-arms
/// the loop, but whatever stopped it (a model that fails every turn, an
/// inbox the agent cannot read) would stop it again.
fn loop_issue(project_root: &Path, scope: &str, name: &str, entry: &AgentEntry) -> Option<Issue> {
    // A stopped loop's reason already carries the last turn's error.
    if let Some(reason) = entry.harness.loop_stopped(project_root, scope, name) {
        return Some(Issue {
            kind: IssueKind::LoopStopped,
            message: format!(
                "agent '{name}' no longer wakes for messages — its never-idle loop \
                 stopped: {reason}. Fix the cause, then `pm agent restart {name}`"
            ),
            fix: Fix::None,
        });
    }
    let error = entry.harness.last_turn_error(project_root, scope, name)?;
    Some(Issue {
        kind: IssueKind::TurnFailed,
        message: format!(
            "agent '{name}' failed its last turn: {error}. Its loop retries, and \
             stops if turns keep failing"
        ),
        fix: Fix::None,
    })
}

/// One warning per active agent in `scope` whose effective definition is
/// `claude`, the removed vanilla alias (spawned by a pre-`default` solo), and
/// per agent registered as `default` before this project's `plain` migration.
fn legacy_vanilla_agent_issues(project_root: &Path, scope: &str) -> Vec<Issue> {
    let Ok(registry) = AgentRegistry::load(&paths::agents_dir(project_root), scope) else {
        return Vec::new();
    };
    let unmigrated = !vanilla_rename::is_migrated(project_root);
    registry
        .agents
        .iter()
        .filter(|(_, entry)| entry.agent_type == AgentType::Agent)
        .filter_map(|(name, entry)| match entry.effective_definition(name) {
            "claude" if entry.active => Some(Issue {
                kind: IssueKind::LegacyVanillaAgentName,
                message: format!(
                    "agent '{name}' uses removed vanilla agent name 'claude' and cannot be \
                     restarted (stop it and respawn as '{VANILLA_AGENT}')"
                ),
                fix: Fix::None,
            }),
            LEGACY_VANILLA_AGENT if unmigrated => Some(Issue {
                kind: IssueKind::LegacyVanillaAgentName,
                message: format!(
                    "agent '{name}' was spawned as vanilla agent '{LEGACY_VANILLA_AGENT}', now \
                     '{VANILLA_AGENT}': it cannot be restarted until it is migrated (run `pm \
                     upgrade`)"
                ),
                fix: Fix::Auto(FixAction::MigrateVanillaAgents),
            }),
            _ => None,
        })
        .collect()
}

/// One warning per `[agents.*]` row keyed `default` while no `default`
/// definition exists: the row matches nothing, and was likely meant as `"*"`
/// (every agent) or `plain` (the vanilla agent).
fn legacy_vanilla_row_issues(project_root: &Path) -> Result<Vec<Issue>> {
    if workflow::definition_exists(
        project_root,
        LEGACY_VANILLA_AGENT,
        paths::home_dir().ok().as_deref(),
    ) {
        return Ok(Vec::new());
    }
    let (project, global) = agents_configs(project_root)?;
    let mut issues = Vec::new();
    for (tier, config) in [("project", &project), ("global", &global)] {
        for (table, rows) in [
            ("harness", &config.harness),
            ("models", &config.models),
            ("permissions", &config.permissions),
        ] {
            if rows.contains_key(LEGACY_VANILLA_AGENT) {
                issues.push(Issue {
                    kind: IssueKind::LegacyVanillaConfigRow,
                    message: format!(
                        "{tier} [agents.{table}] row '{LEGACY_VANILLA_AGENT}' matches no agent \
                         definition: use \"{WILDCARD_AGENT}\" for every agent or \
                         '{VANILLA_AGENT}' for the vanilla agent"
                    ),
                    fix: Fix::None,
                });
            }
        }
    }
    Ok(issues)
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
    use super::*;
    use crate::commands::feat_new;
    use crate::state::project::HarnessConfig;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn bundled_issues_report_dangling_references_and_unknown_names() {
        let root = tempdir().unwrap();
        let global = tempdir().unwrap();
        let home = tempdir().unwrap();
        let wf = global.path().join("research-implement-review");
        std::fs::create_dir_all(&wf).unwrap();
        std::fs::write(
            wf.join("config.toml"),
            "description = \"d\"\nagents = [\"researcher\", \"reviewer\"]\n",
        )
        .unwrap();
        let disabled = Disabled::from_config(
            toml::from_str("agents = [\"researcher\", \"planner\"]\nworkflows = [\"solo\"]")
                .unwrap(),
        );
        let messages = |d: &Disabled| -> Vec<(IssueKind, String)> {
            bundled_issues(root.path(), global.path(), Some(home.path()), d)
                .unwrap()
                .into_iter()
                .map(|i| (i.kind, i.message))
                .collect()
        };

        let issues = messages(&disabled);
        let of = |kind| {
            issues
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, m)| m.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            of(IssueKind::BundledDisabled),
            vec!["disabled by `[bundled.disable]`: Agent 'researcher', Workflow 'solo'"]
        );
        assert_eq!(
            of(IssueKind::BundledUnknown),
            vec!["`[bundled.disable]` names nothing pm bundles: agents 'planner'"]
        );
        let dangling = of(IssueKind::BundledDisabledDangling);
        assert_eq!(dangling.len(), 2, "{dangling:?}");
        assert!(
            dangling[0]
                .starts_with("workflow 'research-implement-review' needs agent 'researcher'"),
            "{dangling:?}"
        );
        assert!(
            dangling[1].contains("needs workflow 'solo'"),
            "{dangling:?}"
        );

        let agents = paths::main_worktree(root.path()).join(".agents/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("researcher.md"), "# mine").unwrap();
        assert!(
            !messages(&disabled)
                .iter()
                .any(|(_, m)| m.contains("needs agent")),
        );
        assert!(messages(&Disabled::default()).is_empty());
    }

    fn unprojected(project_root: &Path) -> Result<Vec<(String, Harness)>> {
        unprojected_definitions(&DefinitionProjections::load(project_root)?)
    }

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
        let main_finding = findings.iter().find(|f| f.feature() == "main").unwrap();
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
        assert_eq!(lines, vec!["Checked main and 0 feature(s): all healthy"]);
    }

    #[test]
    fn no_features_still_reports_main_scope_findings() {
        // Hook and trust findings are about the machine, not a feature, so
        // a project that has none yet must still hear about them.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".to_string(), "codex".to_string());
        config.save(&pm_dir).unwrap();
        // main runs that agent, so its worktree needs codex's trust.
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
                active: false,
                agent_definition: None,
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry
            .save(&paths::agents_dir(&project_path), "main")
            .unwrap();

        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        let main_kinds: Vec<IssueKind> = findings
            .iter()
            .filter(|f| f.feature() == "main")
            .flat_map(|f| f.issues())
            .map(|i| i.kind())
            .collect();
        assert!(
            main_kinds.iter().any(|k| matches!(
                k,
                IssueKind::HooksNotInstalled
                    | IssueKind::HookUntrusted
                    | IssueKind::WorktreeUntrusted
            )),
            "{main_kinds:?}"
        );
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines[0].starts_with("Checked main and 0 feature(s): ")
                && lines[0].contains("issue(s) found"),
            "{lines:?}"
        );
    }

    #[test]
    fn capability_warning_skipped_when_baseline_absent() {
        // No `pm-baseline.md` installed → the capability probe is never run
        // and no warning is produced (so `claude` isn't invoked needlessly).
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        std::fs::create_dir_all(paths::main_worktree(project_root).join(".claude")).unwrap();
        assert!(
            baseline_capability_warnings(project_root, Probe::Fresh)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unprojected_project_definition_is_flagged_until_projected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let planner = paths::main_worktree(&project_path).join(".agents/agents/planner.md");
        std::fs::create_dir_all(planner.parent().unwrap()).unwrap();
        std::fs::write(&planner, "# planner").unwrap();

        assert_eq!(
            unprojected(&project_path).unwrap(),
            vec![("planner".to_string(), Harness::ClaudeCode)]
        );
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("canonical agent 'planner' not projected for claude-code")),
            "{lines:?}"
        );

        // `pm upgrade` projects into main only; the feature needs a pull.
        crate::commands::skills::project_assets(&project_path, false).unwrap();
        assert!(unprojected(&project_path).unwrap().is_empty());
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("not projected"))
                .collect::<Vec<_>>(),
            [
                "  login — canonical agent 'planner' not projected into this worktree for \
              claude-code (run `pm harness pull login`)"
            ],
        );

        doctor(&project_path, &projects_dir, true, server.name()).unwrap();
        assert!(
            project_path
                .join("login/.claude/agents/planner.md")
                .is_file()
        );
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            !lines.iter().any(|l| l.contains("not projected")),
            "{lines:?}"
        );
    }

    /// Put `definition` on opencode, played by a stand-in reporting `version`.
    fn use_opencode(project_path: &Path, definition: &str, version: &str) {
        let pm_dir = paths::pm_dir(project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert(definition.to_string(), "opencode".to_string());
        config.harness.opencode.binary =
            Some(crate::testing::fake_opencode(project_path, version, 0));
        config.save(&pm_dir).unwrap();
    }

    fn messages(issues: &[Issue], kind: IssueKind) -> Vec<String> {
        issues
            .iter()
            .filter(|i| i.kind() == kind)
            .map(|i| i.message().to_string())
            .collect()
    }

    #[test]
    fn opencode_in_use_is_checked_for_its_plugin_and_installed_release() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        let home = dir.path().join("home");

        // Not in use: nothing about opencode, whatever is installed.
        let issues = hook_issues_in(&project_path, &home, Probe::Fresh).unwrap();
        assert!(
            issues.iter().all(|i| !i.message().contains("opencode")),
            "{:?}",
            issues.iter().map(Issue::message).collect::<Vec<_>>()
        );

        use_opencode(&project_path, "reviewer", "opencode v2.0.17");
        let issues = hook_issues_in(&project_path, &home, Probe::Fresh).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::HarnessUnusable),
            vec![
                "agents configured for opencode cannot run: installed opencode is `opencode \
                 v2.0.17`; pm's never-idle plugin needs 2.0.18 or later"
            ]
        );
        let plugin = messages(&issues, IssueKind::HooksNotInstalled)
            .into_iter()
            .filter(|m| m.contains("opencode"))
            .collect::<Vec<_>>();
        assert_eq!(plugin.len(), 1, "{plugin:?}");
        assert!(
            plugin[0].starts_with("pm plugin for opencode missing or out of date in "),
            "{plugin:?}"
        );
        assert!(
            plugin[0].contains(".config/opencode/plugins/pm-never-idle"),
            "{plugin:?}"
        );
        // No trust gate on opencode.
        assert!(
            issues
                .iter()
                .all(|i| i.kind() != IssueKind::WorktreeUntrusted
                    && i.kind() != IssueKind::HookUntrusted)
        );

        use_opencode(&project_path, "reviewer", "opencode v2.0.18");
        for (path, content) in Harness::OpenCode.plugin_files(&home) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let issues = hook_issues_in(&project_path, &home, Probe::Fresh).unwrap();
        assert!(
            issues.iter().all(|i| !i.message().contains("opencode")),
            "{:?}",
            issues.iter().map(Issue::message).collect::<Vec<_>>()
        );

        // A copy an upgrade has not replaced yet.
        let (index, _) = Harness::OpenCode.plugin_files(&home).remove(0);
        std::fs::write(index, "export default {}").unwrap();
        let issues = hook_issues_in(&project_path, &home, Probe::Fresh).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::HooksNotInstalled)
                .iter()
                .filter(|m| m.contains("opencode"))
                .count(),
            1
        );
    }

    #[test]
    fn opencode_agents_without_a_model_row_and_refused_providers_are_reported() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        assert!(harness_config_issues(&project_path).unwrap().is_empty());

        use_opencode(&project_path, "*", "opencode v2.0.18");
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        let agents = &mut config.agents;
        for (definition, harness) in [("reviewer", "opencode"), ("planner", "claude-code")] {
            agents.harness.insert(definition.into(), harness.into());
        }
        agents.models.insert("reviewer".into(), "local/qwen".into());
        config.harness.opencode.providers = [(
            "local".to_string(),
            "settings = { apiKey = \"sk-live-123\" }".parse().unwrap(),
        )]
        .into();
        config.save(&pm_dir).unwrap();
        // Launched in a worktree, on the harness `*` names, with no row.
        let mut registry = AgentRegistry::default();
        registry.register(
            "frontend-dev",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "frontend-dev".to_string(),
                active: false,
                agent_definition: Some("implementer".to_string()),
                harness: Harness::OpenCode,
                spawned_at: None,
            },
        );
        registry
            .save(&paths::agents_dir(&project_path), "main")
            .unwrap();

        let issues = harness_config_issues(&project_path).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::AgentModelMissing),
            [
                "agent 'implementer' runs on opencode and has no [agents.models] row, so it will \
              not spawn"
            ]
        );
        let invalid = messages(&issues, IssueKind::HarnessConfigInvalid);
        assert_eq!(invalid.len(), 1, "{invalid:?}");
        assert!(
            invalid[0].starts_with("[harness.opencode.providers.local] `settings.apiKey` must"),
            "{invalid:?}"
        );
        assert!(!invalid[0].contains("sk-live"), "{invalid:?}");

        // A row that exists but is bound to another harness.
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        let agents = &mut config.agents;
        agents.harness.insert("*".into(), "claude-code".into());
        agents.models.remove("reviewer");
        agents.models.insert("*".into(), "opus".into());
        config.save(&pm_dir).unwrap();
        let issues = harness_config_issues(&project_path).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::AgentModelMissing),
            [
                "agent 'reviewer' runs on opencode and has no [agents.models] row, so it will \
              not spawn (project [agents.models] row for '*' is bound to claude-code, not \
              opencode — not applied)"
            ]
        );
    }

    #[test]
    fn opencode_rows_a_spawn_would_refuse_are_reported() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        use_opencode(&project_path, "reviewer", "opencode v2.0.18");
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .models
            .insert("reviewer".into(), "qwen".into());
        config.save(&pm_dir).unwrap();

        let issues = harness_config_issues(&project_path).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::AgentRowInvalid),
            [
                "agent 'reviewer' will not spawn: [agents.models] row for an opencode agent \
                 must be `<provider>/<model>[#variant]`; got: qwen"
            ]
        );
    }

    #[test]
    fn an_opencode_model_its_provider_does_not_declare_is_reported() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        use_opencode(&project_path, "reviewer", "opencode v2.0.18");
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .models
            .insert("reviewer".into(), "local/qwen-typo".into());
        config.harness.opencode.providers = [(
            "local".to_string(),
            "models = { qwen = {} }".parse().unwrap(),
        )]
        .into();
        config.save(&pm_dir).unwrap();

        let issues = harness_config_issues(&project_path).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::AgentRowRemark),
            ["agent 'reviewer': model 'qwen-typo' is not among those \
                 [harness.opencode.providers.local] declares (qwen); if it is a typo, every turn \
                 fails at the endpoint"]
        );
    }

    #[test]
    fn opencode_definition_without_a_projected_copy_is_flagged() {
        // opencode runs its built-in prompt for a name it cannot find, with
        // no error, so the projected file is the only thing to check.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        use_opencode(&project_path, "planner", "opencode v2.0.18");
        let planner = paths::main_worktree(&project_path).join(".agents/agents/planner.md");
        std::fs::create_dir_all(planner.parent().unwrap()).unwrap();
        std::fs::write(&planner, "# planner").unwrap();

        assert!(
            unprojected(&project_path)
                .unwrap()
                .contains(&("planner".to_string(), Harness::OpenCode))
        );
        let issues = asset_issues(
            &project_path,
            &DefinitionProjections::load(&project_path).unwrap(),
        )
        .unwrap();
        assert!(
            messages(&issues, IssueKind::AssetNotProjected).contains(
                &"canonical agent 'planner' not projected for opencode (run `pm upgrade`)"
                    .to_string()
            ),
            "{:?}",
            issues.iter().map(Issue::message).collect::<Vec<_>>()
        );

        crate::commands::skills::project_assets(&project_path, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(
                paths::main_worktree(&project_path).join(".opencode/agents/planner.md")
            )
            .unwrap(),
            "# planner"
        );
        assert!(
            !unprojected(&project_path)
                .unwrap()
                .iter()
                .any(|(name, _)| name == "planner")
        );
    }

    #[test]
    fn stopped_loop_and_failed_turn_are_reported_until_the_agent_respawns() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        let entry = |harness| crate::state::agent::AgentEntry {
            agent_type: AgentType::Agent,
            session_id: "ses_1".to_string(),
            window_name: "reviewer".to_string(),
            active: true,
            agent_definition: None,
            harness,
            spawned_at: None,
        };
        let opencode = entry(Harness::OpenCode);
        let native = entry(Harness::ClaudeCode);
        let loop_issues =
            |scope, entry| Vec::from_iter(loop_issue(&project_path, scope, "reviewer", entry));

        assert!(loop_issues("login", &opencode).is_empty());

        // Written where the spawn told the plugin to write it.
        let spawn = || {
            Harness::OpenCode
                .pre_launch(
                    &crate::harness::LaunchContext {
                        project_root: &project_path,
                        feature: "login",
                        worktree: &project_path,
                        agent: "reviewer",
                    },
                    &crate::harness::SpawnSpec {
                        resume_session: Some("ses_1"),
                        model: Some("local/qwen"),
                        ..Default::default()
                    },
                    &HarnessConfig {
                        opencode: crate::state::project::OpenCodeConfig {
                            binary: Some(crate::testing::fake_opencode(
                                dir.path(),
                                r#"{"data":{"id":"ses_1"}}"#,
                                0,
                            )),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .unwrap()
        };
        let pre = spawn();
        let (_, trip_file) = pre
            .env
            .iter()
            .find(|(key, _)| key == "PM_OPENCODE_TRIP_FILE")
            .expect("the spawn names a trip file");
        // The plugin writes the turn error beside the trip file.
        let turn_error_file = Path::new(trip_file).with_file_name("opencode.turn-error");

        std::fs::write(
            &turn_error_file,
            "Model unavailable: local/qwen (provider.no-route)\n",
        )
        .unwrap();
        let issues = loop_issues("login", &opencode);
        assert_eq!(
            messages(&issues, IssueKind::TurnFailed),
            vec![
                "agent 'reviewer' failed its last turn: Model unavailable: local/qwen \
                 (provider.no-route). Its loop retries, and stops if turns keep failing"
            ]
        );

        // The trip reason already names the last turn's error.
        std::fs::write(trip_file, "5 consecutive turns read none\n").unwrap();
        let issues = loop_issues("login", &opencode);
        assert_eq!(
            messages(&issues, IssueKind::LoopStopped),
            vec![
                "agent 'reviewer' no longer wakes for messages — its never-idle loop stopped: 5 \
                 consecutive turns read none. Fix the cause, then `pm agent restart reviewer`"
            ]
        );
        assert!(messages(&issues, IssueKind::TurnFailed).is_empty());
        // The same name in another scope, and on a harness with a native hook.
        assert!(loop_issues("signup", &opencode).is_empty());
        assert!(loop_issues("login", &native).is_empty());

        spawn();
        assert!(loop_issues("login", &opencode).is_empty());
    }

    #[test]
    fn stale_bundled_copies_flagged_until_migrated_then_redundant_overrides() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let bundled_reviewer = include_str!("../../../assets/agents/reviewer.md");

        // A pre-migration project: bundled copies present, no marker.
        let claude_agents = paths::main_worktree(&project_path).join(".claude/agents");
        std::fs::create_dir_all(&claude_agents).unwrap();
        std::fs::write(claude_agents.join("reviewer.md"), bundled_reviewer).unwrap();
        std::fs::remove_file(paths::migrations_dir(&project_path).join("global-assets")).unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("pre-migration bundled copies shadow")),
            "{lines:?}"
        );

        crate::commands::skills::migrate_project_to_global(&project_path, false).unwrap();
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            !lines.iter().any(|l| l.contains("pre-migration")),
            "{lines:?}"
        );

        // After the marker, a bundled-named copy is an override — flagged
        // only as redundant when its bytes match the bundle.
        let canonical = paths::main_worktree(&project_path).join(".agents/agents");
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("reviewer.md"), bundled_reviewer).unwrap();
        crate::commands::skills::project_assets(&project_path, false).unwrap();
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("identical to the bundled asset")),
            "{lines:?}"
        );

        std::fs::write(canonical.join("reviewer.md"), "my reviewer").unwrap();
        crate::commands::skills::project_assets(&project_path, false).unwrap();
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            !lines.iter().any(|l| l.contains("identical to the bundled")),
            "{lines:?}"
        );
    }

    #[test]
    fn legacy_claude_agent_name_is_flagged() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        registry.register(
            "claude",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "claude".to_string(),
                active: true,
                agent_definition: None,
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.register(
            "dev",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "dev".to_string(),
                active: true,
                agent_definition: Some("plain".to_string()),
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines.iter().any(|l| l.contains("login")
                && l.contains("agent 'claude' uses removed vanilla agent name")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("agent 'dev' uses removed")),
            "{lines:?}"
        );

        registry.get_mut("claude").unwrap().active = false;
        registry.save(&agents_dir, "login").unwrap();
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            !lines.iter().any(|l| l.contains("uses removed")),
            "{lines:?}"
        );
    }

    #[test]
    fn unmigrated_default_agent_is_flagged_and_fix_migrates_it() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        registry.register(
            "default",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "default".to_string(),
                active: false,
                agent_definition: None,
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, "login").unwrap();
        let flagged = |lines: &[String]| {
            lines
                .iter()
                .any(|l| l.contains("agent 'default' was spawned as vanilla agent"))
        };

        // A project born after the rename: `default` is an ordinary name.
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(!flagged(&lines), "{lines:?}");

        std::fs::remove_file(paths::migrations_dir(&project_path).join("plain-agent")).unwrap();
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(flagged(&lines), "{lines:?}");

        doctor(&project_path, &projects_dir, true, server.name()).unwrap();
        let entry = AgentRegistry::load(&agents_dir, "login").unwrap();
        assert_eq!(
            entry
                .get("default")
                .unwrap()
                .effective_definition("default"),
            VANILLA_AGENT
        );
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(!flagged(&lines), "{lines:?}");
    }

    #[test]
    fn config_row_keyed_default_is_flagged_until_a_default_definition_exists() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .permissions
            .insert("default".into(), "auto".into());
        config.agents.models.insert("plain".into(), "opus".into());
        config.save(&pm_dir).unwrap();

        assert_eq!(
            messages(
                &legacy_vanilla_row_issues(&project_path).unwrap(),
                IssueKind::LegacyVanillaConfigRow
            ),
            [
                "project [agents.permissions] row 'default' matches no agent definition: use \
                 \"*\" for every agent or 'plain' for the vanilla agent"
            ]
        );

        let defs = paths::main_worktree(&project_path).join(".agents/agents");
        std::fs::create_dir_all(&defs).unwrap();
        std::fs::write(defs.join("default.md"), "stub").unwrap();
        assert!(legacy_vanilla_row_issues(&project_path).unwrap().is_empty());
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
        let main_scope = findings.iter().find(|f| f.feature == "main").unwrap();
        assert!(
            main_scope
                .issues
                .iter()
                .any(|i| i.kind() == IssueKind::MainBranchMissing),
            "{:?}",
            main_scope
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
    fn global_config_warning_flags_unparseable_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[project\nmax_features =").unwrap();
        let warning = global_config_warning_in(dir.path()).expect("malformed config must warn");
        // Names both the file and what the user silently lost.
        assert!(warning.contains("config.toml"), "got: {warning}");
        assert!(warning.contains("[agents.models]"), "got: {warning}");
    }

    #[test]
    fn registry_warnings_name_each_unreadable_entry_only() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let good = ProjectEntry {
            root: "/tmp/good".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        good.save(&projects_dir, "good").unwrap();
        let bad = projects_dir.join("bad.toml");
        std::fs::write(&bad, "root = ").unwrap();

        let warnings = registry_warnings(&projects_dir).unwrap();

        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert!(
            warnings[0].contains(&bad.display().to_string()) && warnings[0].contains("'bad'"),
            "got: {}",
            warnings[0]
        );
    }

    #[test]
    fn global_config_warning_silent_when_valid_or_absent() {
        let dir = tempdir().unwrap();
        assert!(global_config_warning_in(dir.path()).is_none());
        std::fs::write(
            dir.path().join("config.toml"),
            "[agents.models]\nreviewer = \"opus\"\n",
        )
        .unwrap();
        assert!(global_config_warning_in(dir.path()).is_none());
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
        let child = findings.iter().find(|f| f.feature == "child").unwrap();
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

    // --- --fix tests ---

    #[test]
    fn codex_in_use_is_checked_for_hooks_trust_and_worktree_trust() {
        let _guard = crate::testing::CODEX_CONFIG_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Trust is per worktree: main runs a registered agent whose
        // definition (not its key) is the reviewer, login's workflow team
        // includes one, api has only an implementer.
        let stopped = |name: &str, definition: Option<&str>| crate::state::agent::AgentEntry {
            agent_type: AgentType::Agent,
            session_id: String::new(),
            window_name: name.to_string(),
            active: false,
            agent_definition: definition.map(str::to_string),
            harness: Harness::ClaudeCode,
            spawned_at: None,
        };
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register("qa", stopped("qa", Some("reviewer")));
        registry.save(&agents_dir, "main").unwrap();
        let features_dir = paths::features_dir(&project_path);
        let mut login = FeatureState::load(&features_dir, "login").unwrap();
        login.workflow = Some("implement-and-review".to_string());
        login.save(&features_dir, "login").unwrap();
        crate::commands::feat_new::feat_new(
            &crate::commands::feat_new::FeatNewParams::with_defaults(
                &project_path,
                &TestServer::registry_dir(&project_path),
                "api",
                server.name(),
            ),
        )
        .unwrap();
        let mut registry = AgentRegistry::default();
        registry.register("implementer", stopped("implementer", None));
        registry.save(&agents_dir, "api").unwrap();

        fn hook_kinds<'a>(issues: impl Iterator<Item = &'a Issue>) -> Vec<(IssueKind, String)> {
            issues
                .filter(|i| {
                    matches!(
                        i.kind(),
                        IssueKind::HooksNotInstalled
                            | IssueKind::HooksMalformed
                            | IssueKind::HookUntrusted
                            | IssueKind::WorktreeUntrusted
                    )
                })
                .map(|i| (i.kind(), i.message().to_string()))
                .collect()
        }
        let kinds = |findings: &[Finding]| -> Vec<(IssueKind, String)> {
            hook_kinds(
                findings
                    .iter()
                    .filter(|f| f.feature() == "main")
                    .flat_map(|f| f.issues().iter()),
            )
        };

        // Claude Code only: nothing codex-related, although the install
        // wrote codex's file too.
        assert!(hooks_install::is_installed_for(Harness::Codex).unwrap());
        assert!(
            kinds(&diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap())
                .is_empty()
        );

        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".to_string(), "codex".to_string());
        config.save(&pm_dir).unwrap();

        // Against a home nothing has installed into (the shared test home
        // is written by every concurrent `init`): hooks missing, worktrees
        // untrusted.
        let bare_home = dir.path().join("bare-home");
        let found = hook_kinds(
            hook_issues_in(&project_path, &bare_home, Probe::Fresh)
                .unwrap()
                .iter(),
        );
        assert_eq!(
            found.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![
                IssueKind::HooksNotInstalled,
                IssueKind::HooksNotInstalled,
                IssueKind::WorktreeUntrusted,
                IssueKind::WorktreeUntrusted
            ],
            "{found:?}"
        );
        assert!(found[0].1.contains(".claude/settings.json"), "{found:?}");
        assert!(found[1].1.contains(".codex/hooks.json"), "{found:?}");
        assert!(
            found[2].1.starts_with("main is not trusted by codex"),
            "{found:?}"
        );
        assert!(
            found[3].1.starts_with("login is not trusted by codex"),
            "{found:?}"
        );

        // --fix installs the hooks and trusts both worktrees; hook trust is
        // codex's alone to grant, so it remains.
        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("not trusted by codex")),
            "{lines:?}"
        );
        assert!(hooks_install::is_installed_for(Harness::Codex).unwrap());
        let found =
            kinds(&diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap());
        assert_eq!(
            found.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![IssueKind::HookUntrusted; 4],
            "{found:?}"
        );
        assert!(found[0].1.contains("pm's Stop hook"), "{found:?}");
        assert!(found[1].1.contains("pm's SessionStart hook"), "{found:?}");
        assert!(
            found[2].1.contains("pm's UserPromptSubmit hook"),
            "{found:?}"
        );
        assert!(
            found[3].1.contains(
                "pm's status hooks (PermissionRequest, PreToolUse, PostToolUse, Interrupt), \
                 so an agent waiting on you reads as busy"
            ),
            "{found:?}"
        );

        // Trust recorded the way codex writes it, at pm's entries' positions,
        // in a home of the test's own: trust written to the shared test home
        // would outlive this test and reach every other one.
        let trusted_home = dir.path().join("trusted-home");
        hooks_install::install_in(&trusted_home, None, false).unwrap();
        for wt in ["main", "login"] {
            Harness::Codex
                .trust_worktree(&trusted_home, &project_path.join(wt))
                .unwrap();
        }
        let codex_hooks = trusted_home.join(".codex/hooks.json");
        let root = hooks_install::user_hooks_root(Harness::Codex, &trusted_home)
            .unwrap()
            .unwrap();
        let mut trust = String::new();
        for (event, markers) in hooks_install::pm_events(Harness::Codex) {
            let (i, j) = hooks_install::pm_hook_position(&root, event, markers).unwrap();
            let snake = match event {
                "Stop" => "stop",
                "SessionStart" => "session_start",
                "UserPromptSubmit" => "user_prompt_submit",
                "PermissionRequest" => "permission_request",
                "PreToolUse" => "pre_tool_use",
                "PostToolUse" => "post_tool_use",
                "Interrupt" => "interrupt",
                other => panic!("no codex trust key for {other}"),
            };
            trust.push_str(&format!(
                "[hooks.state.\"{}:{snake}:{i}:{j}\"]\ntrusted_hash = \"sha256:t\"\n",
                codex_hooks.display()
            ));
        }
        let config_toml = trusted_home.join(".codex/config.toml");
        let existing = std::fs::read_to_string(&config_toml).unwrap();
        std::fs::write(&config_toml, format!("{existing}\n{trust}")).unwrap();
        let found = hook_kinds(
            hook_issues_in(&project_path, &trusted_home, Probe::Fresh)
                .unwrap()
                .iter(),
        );
        assert!(found.is_empty(), "{found:?}");

        // A flat hooks.json registers nothing in codex: flagged, not "installed".
        let flat = bare_home.join(".codex/hooks.json");
        std::fs::create_dir_all(flat.parent().unwrap()).unwrap();
        std::fs::write(
            &flat,
            r#"{"hooks":{"Stop":[{"type":"command","command":"pm harness hooks stop"}]}}"#,
        )
        .unwrap();
        let found = hook_kinds(
            hook_issues_in(&project_path, &bare_home, Probe::Fresh)
                .unwrap()
                .iter(),
        );
        assert_eq!(
            found.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![
                IssueKind::HooksNotInstalled,
                IssueKind::HooksMalformed,
                IssueKind::HooksNotInstalled,
                IssueKind::WorktreeUntrusted,
                IssueKind::WorktreeUntrusted
            ],
            "{found:?}"
        );
        assert!(found[2].1.contains(".codex/hooks.json"), "{found:?}");
    }

    #[test]
    fn fix_strips_stale_project_hooks() {
        // A project last upgraded by a release that wrote the hooks into
        // main/.claude/settings.json (and seeded them into the feature).
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let legacy = r#"{"permissions":{"allow":["Read"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"pm claude hooks stop"}]}]}}"#;
        for wt in ["main", "login"] {
            let claude = project_path.join(wt).join(".claude");
            std::fs::create_dir_all(&claude).unwrap();
            std::fs::write(claude.join("settings.json"), legacy).unwrap();
        }

        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        let main = findings.iter().find(|f| f.feature() == "main").unwrap();
        let stale: Vec<&str> = main
            .issues()
            .iter()
            .filter(|i| i.kind() == IssueKind::StaleProjectHooks)
            .map(|i| i.message())
            .collect();
        assert_eq!(stale.len(), 2, "{stale:?}");
        assert!(stale[0].contains("main/.claude/settings.json"), "{stale:?}");
        assert!(
            stale[1].contains("login/.claude/settings.json"),
            "{stale:?}"
        );
        assert!(
            !main
                .issues()
                .iter()
                .any(|i| i.kind() == IssueKind::HooksNotInstalled)
        );

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("pm hooks still in")),
            "got: {lines:?}"
        );
        for wt in ["main", "login"] {
            let settings: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(project_path.join(wt).join(".claude/settings.json"))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(settings["permissions"]["allow"][0], "Read", "{wt}");
            assert!(settings.get("hooks").is_none(), "{wt}: {settings}");
        }
        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        assert!(
            findings.iter().all(|f| f.feature() != "main"),
            "main still has issues after fix"
        );
    }

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

    #[test]
    fn detects_dead_agent_window_in_existing_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Register an agent as active, but don't create its window
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
                active: true,
                agent_definition: None,
                harness: crate::harness::Harness::OpenCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, "login").unwrap();
        // A loop stopped in the dead window is moot until it respawns.
        let runtime = crate::state::runtime::agent_dir(&project_path, "login", "reviewer").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("opencode.tripped"),
            "5 consecutive turns read none\n",
        )
        .unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("agent 'reviewer'") && l.contains("window missing")),
            "got: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("no longer wakes")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn fix_respawns_dead_agent() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Register an agent as active, but don't create its window
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
                active: true,
                agent_definition: None,
                harness: crate::harness::Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("agent 'reviewer'")),
            "got: {lines:?}"
        );

        // Agent window should now exist
        assert!(
            tmux::find_window(
                server.name(),
                &tmux::session_name(&project_name, "login"),
                "reviewer"
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn fix_respawn_reports_spawn_notes() {
        let _guard = crate::testing::CODEX_CONFIG_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: "cc-session".to_string(),
                window_name: "reviewer".to_string(),
                active: true,
                agent_definition: None,
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, "login").unwrap();
        // Config moves reviewer off the harness its session was recorded under.
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".to_string(), "codex".to_string());
        config.save(&pm_dir).unwrap();

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines.iter().any(|l| l.contains(
                "login — fixed: agent 'reviewer' registered as active but window missing \
                 (harness changed claude-code → codex; previous session not resumed)"
            )),
            "got: {lines:?}"
        );
    }

    #[test]
    fn healthy_agent_not_flagged() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Register an agent AND create its window with a non-shell process
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session_name, "login", "reviewer");

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(lines[0].contains("all healthy"), "got: {lines:?}");
    }

    #[test]
    fn running_agent_launched_with_what_has_changed_is_flagged() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session_name, "login", "reviewer");
        crate::state::runtime::write_launch_stamp(&project_path, "login", "reviewer", "old")
            .unwrap();

        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        let stale: Vec<&str> = findings
            .iter()
            .flat_map(Finding::issues)
            .filter(|i| i.kind() == IssueKind::AgentLaunchStale)
            .map(Issue::message)
            .collect();
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(
            stale[0].contains("`pm agent restart reviewer --scope login`"),
            "{stale:?}"
        );
    }

    #[test]
    fn running_opencode_agent_whose_plugin_never_loaded_is_flagged_once_past_grace() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session_name, "login", "reviewer");

        let agents_dir = paths::agents_dir(&project_path);
        let save = |edit: &dyn Fn(&mut AgentEntry)| {
            let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
            edit(registry.get_mut("reviewer").unwrap());
            registry.save(&agents_dir, "login").unwrap();
        };
        let login_issues = || -> Vec<(IssueKind, String)> {
            diagnose(&project_path, &projects_dir, server.name(), Depth::Quick)
                .unwrap()
                .iter()
                .filter(|f| f.feature() == "login")
                .flat_map(|f| f.issues())
                .map(|i| (i.kind(), i.message().to_string()))
                .collect()
        };

        // The session id is recorded before the TUI starts, so only the
        // plugin's own marker shows it loaded.
        save(&|e| {
            e.harness = Harness::OpenCode;
            e.session_id = "ses_1".to_string();
            e.spawned_at = Some(chrono::Utc::now());
        });
        assert_eq!(login_issues(), vec![]);

        save(&|e| e.spawned_at = Some(chrono::Utc::now() - 2 * START_GRACE));
        assert_eq!(
            login_issues(),
            vec![(
                IssueKind::LoopNotLoaded,
                "agent 'reviewer' is running but opencode has not loaded pm's never-idle plugin, \
                 so it never wakes for messages (`/plugins` in its window shows why; then `pm \
                 agent restart reviewer --scope login`)"
                    .to_string()
            )]
        );

        // Where the plugin writes it.
        let runtime = crate::state::runtime::agent_dir(&project_path, "login", "reviewer").unwrap();
        std::fs::write(runtime.join("opencode.loaded"), "2026-10-01T00:00:00Z\n").unwrap();
        assert_eq!(login_issues(), vec![]);

        // A native hook has nothing to load.
        std::fs::remove_file(runtime.join("opencode.loaded")).unwrap();
        save(&|e| e.harness = Harness::ClaudeCode);
        assert_eq!(login_issues(), vec![]);
    }

    #[test]
    fn an_open_window_whose_harness_exited_is_flagged_instead_of_its_missing_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_dead_fake_agent(&project_path, &session_name, "login", "reviewer");
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        let entry = registry.get_mut("reviewer").unwrap();
        entry.harness = Harness::Codex;
        entry.spawned_at = Some(chrono::Utc::now() - 2 * START_GRACE);
        registry.save(&agents_dir, "login").unwrap();

        let issues: Vec<(IssueKind, String)> =
            diagnose(&project_path, &projects_dir, server.name(), Depth::Quick)
                .unwrap()
                .iter()
                .filter(|f| f.feature() == "login")
                .flat_map(|f| f.issues())
                .map(|i| (i.kind(), i.message().to_string()))
                .collect();
        assert_eq!(
            issues,
            vec![(
                IssueKind::AgentHarnessExited,
                "agent 'reviewer' has its window open but its codex harness exited (its window \
                 shows why; fix that, then `pm agent restart reviewer --scope login`)"
                    .to_string()
            )]
        );
    }

    #[test]
    fn running_agent_with_no_session_id_is_flagged_once_past_its_own_grace_period() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session_name, "login", "reviewer");
        server.spawn_fake_agent(&project_path, &session_name, "login", "implementer");

        let agents_dir = paths::agents_dir(&project_path);
        let long_ago = chrono::Utc::now() - 2 * START_GRACE;
        let save = |edit: &dyn Fn(&mut AgentEntry)| {
            let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
            edit(registry.get_mut("reviewer").unwrap());
            registry.save(&agents_dir, "login").unwrap();
        };
        let login_issues = || -> Vec<(IssueKind, String)> {
            diagnose(&project_path, &projects_dir, server.name(), Depth::Quick)
                .unwrap()
                .iter()
                .filter(|f| f.feature() == "login")
                .flat_map(|f| f.issues())
                .map(|i| (i.kind(), i.message().to_string()))
                .collect()
        };

        // Just spawned: the hook may simply not have fired yet.
        save(&|e| {
            e.harness = Harness::Codex;
            e.spawned_at = Some(chrono::Utc::now());
        });
        assert_eq!(login_issues(), vec![]);

        // The registry was written a moment ago, as it is whenever another
        // agent in the scope records its session; the reviewer's own spawn
        // is what counts.
        save(&|e| e.spawned_at = Some(long_ago));
        assert_eq!(
            login_issues(),
            vec![(
                IssueKind::AgentSessionNotStarted,
                "agent 'reviewer' is running but its codex session has recorded no session id \
                 (run `pm agent restart reviewer --scope login`)"
                    .to_string()
            )]
        );

        save(&|e| e.active = false);
        assert_eq!(login_issues(), vec![]);

        save(&|e| {
            e.active = true;
            e.session_id = "sess-1".to_string();
        });
        assert_eq!(login_issues(), vec![]);

        // An entry with no spawn time is as old as the registry file.
        save(&|e| {
            e.session_id.clear();
            e.spawned_at = None;
        });
        assert_eq!(login_issues(), vec![]);
        std::fs::File::options()
            .write(true)
            .open(agents_dir.join("login.toml"))
            .unwrap()
            .set_modified(SystemTime::from(long_ago))
            .unwrap();
        let kinds: Vec<IssueKind> = login_issues().into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            kinds,
            vec![
                IssueKind::AgentSessionNotStarted,
                IssueKind::AgentSessionNotStarted
            ]
        );
    }
}
