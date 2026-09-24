use std::path::{Path, PathBuf};

use crate::commands::feat_delete::{self, CleanupParams};
use crate::commands::{agent_spawn, hooks_install, skills};
use crate::error::Result;
use crate::harness::Harness;
use crate::state::agent::{AgentRegistry, AgentType};
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig};
use crate::state::workflow;
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
    /// A project override whose content equals the bundled asset it shadows.
    RedundantOverride,
    /// A project custom skill the harness resolves its global namesake over.
    SkillShadowedByGlobal,
    /// An active agent is named `claude`, the removed vanilla alias: it runs
    /// until its window dies, then restart/heal fail to resolve a definition.
    LegacyVanillaAgentName,
    /// A harness's hooks file has an entry in a shape the harness silently
    /// registers nothing for.
    HooksMalformed,
    /// The harness has not recorded trust for a pm hook, so it silently does
    /// not run it.
    HookUntrusted,
    /// A worktree is not trusted by the harness, so it stops at an
    /// interactive prompt on launch.
    WorktreeUntrusted,
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
        base: String,
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

/// Run all diagnostic checks without applying any fixes.
///
/// For each feature, checks:
/// 1. Worktree directory exists on disk
/// 2. Git worktree list includes it
/// 3. Branch exists locally
/// 4. Tmux session exists
/// 5. Status stuck on "initializing"
/// 6. Referenced workflow directory exists
/// 7. If PR linked and `check_pr_state` is true, check GH status drift
///
/// Also runs main-scope checks (Stop hook installed, main session present, main
/// agent windows alive) when there is at least one feature, mirroring the
/// existing `doctor` behaviour.
///
/// `check_pr_state` controls whether to make `gh pr view` network calls (one
/// per feature with a linked PR). `pm doctor` passes `true`; latency-sensitive
/// callers like the pre-open warning hook pass `false` to avoid round-trips
/// on every session reopen.
///
/// Returns one [`Finding`] per scope that has issues (or per scope, including
/// healthy ones — callers can filter by inspecting [`Finding::issues`]).
pub fn diagnose(
    project_root: &Path,
    tmux_server: Option<&str>,
    check_pr_state: bool,
) -> Result<Vec<Finding>> {
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
    main_issues.extend(hook_issues(project_root)?);
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
    main_issues.extend(asset_issues(project_root)?);
    main_issues.extend(legacy_vanilla_agent_issues(project_root, "main"));
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
        // Check main-scope agent windows
        let agents_dir = paths::agents_dir(project_root);
        if let Ok(registry) = AgentRegistry::load(&agents_dir, "main") {
            for (agent_name, entry) in &registry.agents {
                if entry.agent_type != AgentType::Agent || !entry.active {
                    continue;
                }
                if tmux::find_window(tmux_server, &main_session, &entry.window_name)?.is_none() {
                    main_issues.push(Issue {
                        kind: IssueKind::AgentWindowMissing,
                        message: format!(
                            "agent '{agent_name}' registered as active but window missing"
                        ),
                        fix: Fix::Auto(FixAction::RespawnAgent {
                            agent_name: agent_name.clone(),
                        }),
                    });
                }
            }
        }
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
                // Check 4b: agent windows alive within existing session
                let agents_dir = paths::agents_dir(project_root);
                if let Ok(registry) = AgentRegistry::load(&agents_dir, name) {
                    for (agent_name, entry) in &registry.agents {
                        if entry.agent_type != AgentType::Agent || !entry.active {
                            continue;
                        }
                        if tmux::find_window(tmux_server, &session_name, &entry.window_name)?
                            .is_none()
                        {
                            issues.push(Issue {
                                kind: IssueKind::AgentWindowMissing,
                                message: format!(
                                    "agent '{agent_name}' registered as active but window missing"
                                ),
                                fix: Fix::Auto(FixAction::RespawnAgent {
                                    agent_name: agent_name.clone(),
                                }),
                            });
                        }
                    }
                }
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
                    base: state.base_or_default().to_string(),
                }),
            });
        }

        // Check 6: referenced workflow directory exists. Non-fatal warning —
        // the workflow may have been deleted by accident or not installed on
        // this machine. No auto-fix: a custom workflow can't be regenerated.
        if let Some(wf) = &state.workflow
            && !workflow::exists(project_root, wf)
        {
            issues.push(Issue {
                kind: IssueKind::WorkflowDirMissing,
                message: format!(
                    "workflow '{wf}' directory missing from .pm/workflows/ \
                     (deleted or not installed on this machine)"
                ),
                fix: Fix::None,
            });
        }

        issues.extend(legacy_vanilla_agent_issues(project_root, name));

        // Check 7: PR status drift (skipped when `check_pr_state` is false to
        // avoid network round-trips on latency-sensitive callers like
        // `pm open`'s pre-recreate warning hook).
        if check_pr_state && !state.pr.is_empty() {
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

/// Run a health check on all features in the project.
///
/// Wraps [`diagnose`] with formatting and (optionally) auto-fix logic.
/// Always passes `check_pr_state = true`, so PR drift is reported here.
///
/// With `fix == true`, auto-resolves clear-cut issues and skips ambiguous ones.
///
/// Returns formatted diagnostic lines.
pub fn doctor(project_root: &Path, fix: bool, tmux_server: Option<&str>) -> Result<Vec<String>> {
    // Project-independent warnings, appended after the status lines.
    let mut warnings = baseline_capability_warnings(project_root)?;
    warnings.extend(global_config_warning());

    // Empty only when the project has no features and the main scope is
    // clean; main-scope findings are reported even with no features.
    let findings = diagnose(project_root, tmux_server, true)?;
    if findings.is_empty() {
        // Status line first, warnings after — matches the ordering in the
        // normal path below.
        let mut lines = vec!["No features to check".to_string()];
        lines.extend(warnings);
        return Ok(lines);
    }

    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;
    let features_dir = paths::features_dir(project_root);
    let main_repo = paths::main_worktree(project_root);

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
    lines.extend(warnings);

    Ok(lines)
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

/// Warn when the shared agent baseline is installed for this project but a
/// harness in use can't deliver pm's composed prompt — how the baseline
/// reaches an agent at spawn time. Nothing when the baseline isn't installed
/// (nothing to apply) or a binary can't be probed.
fn baseline_capability_warnings(project_root: &Path) -> Result<Vec<String>> {
    if !crate::commands::skills::baseline_path(project_root).exists() {
        return Ok(Vec::new());
    }
    Ok(skills::harnesses_in_use(project_root)?
        .into_iter()
        .filter(|h| h.supports_prompt_delivery() == Some(false))
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
pub fn probe_line(harness: Harness) -> String {
    let mechanism = harness.prompt_mechanism();
    match harness.supports_prompt_delivery() {
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
fn hook_issues(project_root: &Path) -> Result<Vec<Issue>> {
    hook_issues_in(project_root, &paths::home_dir()?)
}

/// [`hook_issues`] against an explicit `home`.
fn hook_issues_in(project_root: &Path, home: &Path) -> Result<Vec<Issue>> {
    let worktree_harnesses = worktree_harnesses(project_root)?;
    let mut issues = Vec::new();
    for harness in skills::harnesses_in_use(project_root)? {
        let file = hooks_install::user_settings_path(harness, home)?;
        let shown = crate::path_utils::to_portable(&file);
        let mut installed = false;
        if let Some(root) = hooks_install::user_hooks_root(harness, home)? {
            for event in harness.malformed_hook_events(&root) {
                issues.push(Issue {
                    kind: IssueKind::HooksMalformed,
                    message: format!(
                        "{shown} `hooks.{event}` holds a bare hook object; {harness} registers \
                         nothing for it — wrap it as {{\"hooks\": [...]}}"
                    ),
                    fix: Fix::None,
                });
            }
            installed = true;
            for &(event, markers) in hooks_install::PM_EVENTS {
                let Some((entry, hook)) = hooks_install::pm_hook_position(&root, event, markers)
                else {
                    installed = false;
                    continue;
                };
                if !harness.hook_trusted(home, event, entry, hook) {
                    issues.push(Issue {
                        kind: IssueKind::HookUntrusted,
                        message: format!(
                            "{harness} has not trusted pm's {event} hook, so it silently does \
                             not run: {}",
                            harness.hook_trust_remedy()
                        ),
                        fix: Fix::None,
                    });
                }
            }
        }
        if !installed {
            issues.push(Issue {
                kind: IssueKind::HooksNotInstalled,
                message: format!(
                    "pm hooks not installed in {shown} (run `pm harness hooks install`)"
                ),
                fix: Fix::Auto(FixAction::InstallStopHook),
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

/// Each worktree on disk with the harnesses its agents launch on: those of
/// its registered agents plus, for a feature, its workflow team — the set a
/// spawn there would need the worktree trusted by.
fn worktree_harnesses(project_root: &Path) -> Result<Vec<(PathBuf, Vec<Harness>)>> {
    let project = match ProjectConfig::load(&paths::pm_dir(project_root)) {
        Ok(config) => config.agents,
        Err(crate::error::PmError::NotInProject) => Default::default(),
        Err(e) => return Err(e),
    };
    let global = GlobalConfig::load_or_default().agents;
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
fn asset_issues(project_root: &Path) -> Result<Vec<Issue>> {
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

    for (name, harness) in unprojected_definitions(project_root)? {
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

/// Agent definitions with no projected copy in a harness's own definition
/// dir, in either tier. Such a definition passes `WorkflowDef::validate`
/// but the harness can't find it at launch.
fn unprojected_definitions(project_root: &Path) -> Result<Vec<(String, Harness)>> {
    let main = paths::main_worktree(project_root);
    let canonical = main.join(skills::CANONICAL_DIR).join("agents");
    let harnesses: Vec<Harness> = skills::harnesses_in_use(project_root)?
        .into_iter()
        .filter(|h| h.projects_definitions())
        .collect();
    let mut out = skills::unprojected_global_definitions(&harnesses)?;
    for file in skills::definition_files(&canonical)? {
        for harness in &harnesses {
            if !main
                .join(harness.config_dir())
                .join("agents")
                .join(&file)
                .exists()
            {
                out.push((file.trim_end_matches(".md").to_string(), *harness));
            }
        }
    }
    Ok(out)
}

/// One warning per active agent in `scope` whose effective definition is
/// `claude`, the removed vanilla alias (spawned by a pre-`default` solo).
fn legacy_vanilla_agent_issues(project_root: &Path, scope: &str) -> Vec<Issue> {
    let Ok(registry) = AgentRegistry::load(&paths::agents_dir(project_root), scope) else {
        return Vec::new();
    };
    registry
        .agents
        .iter()
        .filter(|(name, entry)| {
            entry.agent_type == AgentType::Agent
                && entry.active
                && entry.effective_definition(name) == "claude"
        })
        .map(|(name, _)| Issue {
            kind: IssueKind::LegacyVanillaAgentName,
            message: format!(
                "agent '{name}' uses removed vanilla agent name 'claude' and cannot be \
                 restarted (stop it and respawn as 'default')"
            ),
            fix: Fix::None,
        })
        .collect()
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
        FixAction::CleanupInitializing {
            worktree,
            branch,
            base,
        } => {
            let worktree_path = project_root.join(worktree);
            feat_delete::cleanup_feature(&CleanupParams {
                repo: main_repo,
                worktree_path: &worktree_path,
                branch,
                features_dir,
                name,
                project_name,
                force_worktree: true,
                tmux_server,
                delete_branch: true,
                best_effort: false,
                base,
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
        FixAction::RecreateWorktree {
            worktree_path,
            branch,
        } => {
            git::add_worktree(main_repo, worktree_path, branch)?;
        }
        FixAction::InstallStopHook => {
            hooks_install::install(Some(project_root))?;
        }
        FixAction::InstallGlobalAssets => {
            crate::commands::skills::install_global()?;
        }
        FixAction::RespawnAgent { agent_name } => {
            agent_spawn::agent_spawn(project_root, name, agent_name, None, None, tmux_server)?;
        }
        FixAction::TrustWorktree { harness, path } => {
            harness.trust_worktree(&paths::home_dir()?, path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feat_new;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn healthy_feature_reports_ok() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

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
        let (project_path, _, _) = server.setup_project(dir.path());

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert_eq!(lines, vec!["No features to check"]);
    }

    #[test]
    fn no_features_still_reports_main_scope_findings() {
        // Hook and trust findings are about the machine, not a feature, so
        // a project that has none yet must still hear about them.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
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
            },
        );
        registry
            .save(&paths::agents_dir(&project_path), "main")
            .unwrap();

        let findings = diagnose(&project_path, server.name(), false).unwrap();
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
        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert_ne!(lines, vec!["No features to check"]);
        assert!(lines[0].contains("issue(s) found"), "{lines:?}");
    }

    #[test]
    fn capability_warning_skipped_when_baseline_absent() {
        // No `pm-baseline.md` installed → the capability probe is never run
        // and no warning is produced (so `claude` isn't invoked needlessly).
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        std::fs::create_dir_all(paths::main_worktree(project_root).join(".claude")).unwrap();
        assert!(
            baseline_capability_warnings(project_root)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unprojected_project_definition_is_flagged_until_projected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let planner = paths::main_worktree(&project_path).join(".agents/agents/planner.md");
        std::fs::create_dir_all(planner.parent().unwrap()).unwrap();
        std::fs::write(&planner, "# planner").unwrap();

        assert_eq!(
            unprojected_definitions(&project_path).unwrap(),
            vec![("planner".to_string(), Harness::ClaudeCode)]
        );
        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("canonical agent 'planner' not projected for claude-code")),
            "{lines:?}"
        );

        crate::commands::skills::project_assets(&project_path, false).unwrap();
        assert!(unprojected_definitions(&project_path).unwrap().is_empty());
        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            !lines.iter().any(|l| l.contains("not projected")),
            "{lines:?}"
        );
    }

    #[test]
    fn stale_bundled_copies_flagged_until_migrated_then_redundant_overrides() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let bundled_reviewer = include_str!("../../agents/reviewer.md");

        // A pre-migration project: bundled copies present, no marker.
        let claude_agents = paths::main_worktree(&project_path).join(".claude/agents");
        std::fs::create_dir_all(&claude_agents).unwrap();
        std::fs::write(claude_agents.join("reviewer.md"), bundled_reviewer).unwrap();
        std::fs::remove_file(paths::migrations_dir(&project_path).join("global-assets")).unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("pre-migration bundled copies shadow")),
            "{lines:?}"
        );

        crate::commands::skills::migrate_project_to_global(&project_path, false).unwrap();
        let lines = doctor(&project_path, false, server.name()).unwrap();
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
        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("identical to the bundled asset")),
            "{lines:?}"
        );

        std::fs::write(canonical.join("reviewer.md"), "my reviewer").unwrap();
        crate::commands::skills::project_assets(&project_path, false).unwrap();
        let lines = doctor(&project_path, false, server.name()).unwrap();
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
            },
        );
        registry.register(
            "dev",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "dev".to_string(),
                active: true,
                agent_definition: Some("default".to_string()),
                harness: Harness::ClaudeCode,
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
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
        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            !lines.iter().any(|l| l.contains("uses removed")),
            "{lines:?}"
        );
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
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Deregister worktree from git but leave directory on disk
        let main_repo = paths::main_worktree(&project_path);
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
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Remove worktree first (branch can't be deleted while checked out), then branch
        let main_repo = paths::main_worktree(&project_path);
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
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        // Kill the feature's tmux session
        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
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

    #[test]
    fn missing_workflow_directory_detected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Point the feature at a workflow whose directory does not exist.
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.workflow = Some("ghost-workflow".to_string());
        state.save(&features_dir, "login").unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
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

        // Create a workflow directory and point the feature at it.
        let wf_dir = paths::workflows_dir(&project_path).join("real-workflow");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(wf_dir.join("config.toml"), "description = \"x\"\n").unwrap();
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.workflow = Some("real-workflow".to_string());
        state.save(&features_dir, "login").unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
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
        let (project_path, _, _) = server.setup_project(dir.path());
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "alpha",
            server.name(),
        ))
        .unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "beta",
            server.name(),
        ))
        .unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(lines[0].contains("2 feature(s)"), "got: {:?}", lines);
        assert!(lines.iter().any(|l| l.contains("alpha")));
        assert!(lines.iter().any(|l| l.contains("beta")));
    }

    #[test]
    fn multiple_issues_on_same_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        // Remove worktree + branch + tmux session — fully orphaned
        let main_repo = paths::main_worktree(&project_path);
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        git::delete_branch(&main_repo, "login").unwrap();
        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

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
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        // Remove only the worktree directory — branch still exists, so not orphaned
        std::fs::remove_dir_all(project_path.join("login")).unwrap();
        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

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
    fn codex_in_use_is_checked_for_hooks_trust_and_worktree_trust() {
        let _guard = crate::testing::CODEX_CONFIG_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let home = paths::home_dir().unwrap();
        let codex_hooks = home.join(".codex/hooks.json");

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
        assert!(kinds(&diagnose(&project_path, server.name(), false).unwrap()).is_empty());

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
        let found = hook_kinds(hook_issues_in(&project_path, &bare_home).unwrap().iter());
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
        let lines = doctor(&project_path, true, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("not trusted by codex")),
            "{lines:?}"
        );
        assert!(hooks_install::is_installed_for(Harness::Codex).unwrap());
        let found = kinds(&diagnose(&project_path, server.name(), false).unwrap());
        assert_eq!(
            found.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![IssueKind::HookUntrusted, IssueKind::HookUntrusted],
            "{found:?}"
        );
        assert!(found[0].1.contains("pm's Stop hook"), "{found:?}");
        assert!(found[1].1.contains("pm's SessionStart hook"), "{found:?}");

        // Trust recorded the way codex writes it, at pm's entries' positions.
        let root = hooks_install::user_hooks_root(Harness::Codex, &home)
            .unwrap()
            .unwrap();
        let mut trust = String::new();
        for &(event, markers) in hooks_install::PM_EVENTS {
            let (i, j) = hooks_install::pm_hook_position(&root, event, markers).unwrap();
            let snake = if event == "Stop" {
                "stop"
            } else {
                "session_start"
            };
            trust.push_str(&format!(
                "[hooks.state.\"{}:{snake}:{i}:{j}\"]\ntrusted_hash = \"sha256:t\"\n",
                codex_hooks.display()
            ));
        }
        let config_toml = home.join(".codex/config.toml");
        let existing = std::fs::read_to_string(&config_toml).unwrap();
        std::fs::write(&config_toml, format!("{existing}\n{trust}")).unwrap();
        assert!(kinds(&diagnose(&project_path, server.name(), false).unwrap()).is_empty());

        // A flat hooks.json registers nothing in codex: flagged, not "installed".
        let flat = bare_home.join(".codex/hooks.json");
        std::fs::create_dir_all(flat.parent().unwrap()).unwrap();
        std::fs::write(
            &flat,
            r#"{"hooks":{"Stop":[{"type":"command","command":"pm harness hooks stop"}]}}"#,
        )
        .unwrap();
        let found = hook_kinds(hook_issues_in(&project_path, &bare_home).unwrap().iter());
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
        let legacy = r#"{"permissions":{"allow":["Read"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"pm claude hooks stop"}]}]}}"#;
        for wt in ["main", "login"] {
            let claude = project_path.join(wt).join(".claude");
            std::fs::create_dir_all(&claude).unwrap();
            std::fs::write(claude.join("settings.json"), legacy).unwrap();
        }

        let findings = diagnose(&project_path, server.name(), false).unwrap();
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

        let lines = doctor(&project_path, true, server.name()).unwrap();
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
        let findings = diagnose(&project_path, server.name(), false).unwrap();
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
        let session_name = tmux::session_name(&project_name, "login");

        tmux::kill_session(server.name(), &session_name).unwrap();
        assert!(!tmux::has_session(server.name(), &session_name).unwrap());

        let lines = doctor(&project_path, true, server.name()).unwrap();
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

        // Remove worktree, branch, and tmux session — leaving only the state file
        let main_repo = paths::main_worktree(&project_path);
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        git::delete_branch(&main_repo, "login").unwrap();
        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

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
    fn fix_recreates_missing_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Remove worktree directory — branch still exists, so doctor can recreate
        let main_repo = paths::main_worktree(&project_path);
        git::remove_worktree_force(&main_repo, &project_path.join("login")).unwrap();
        assert!(!project_path.join("login").exists());

        let lines = doctor(&project_path, true, server.name()).unwrap();
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

        tmux::kill_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap();

        let lines = doctor(&project_path, true, server.name()).unwrap();
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
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("agent 'reviewer'") && l.contains("window missing")),
            "got: {lines:?}"
        );
    }

    #[test]
    fn fix_respawns_dead_agent() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

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
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        let lines = doctor(&project_path, true, server.name()).unwrap();
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
    fn healthy_agent_not_flagged() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        // Register an agent AND create its window with a non-shell process
        let session_name = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session_name, "login", "reviewer");

        let lines = doctor(&project_path, false, server.name()).unwrap();
        assert!(lines[0].contains("all healthy"), "got: {lines:?}");
    }
}
