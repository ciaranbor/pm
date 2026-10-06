//! `pm doctor`: health checks over a project's scopes, and the fixes for
//! the clear-cut ones. Each submodule owns one family of checks; this
//! module owns the finding types they share.

mod agents;
mod assets;
mod config;
mod hooks;
mod report;
mod scope;
mod warnings;

pub(crate) use agents::START_GRACE;
pub use report::{Report, doctor, offline};
pub use scope::diagnose;
pub use warnings::probe_line;

use std::path::PathBuf;

use crate::harness::{Harness, Probe};
use crate::state::feature::FeatureStatus;

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

/// Helpers the submodules' tests share.
#[cfg(test)]
mod test_support {
    use super::{Issue, IssueKind};
    use crate::state::paths;
    use crate::state::project::ProjectConfig;
    use std::path::Path;

    /// Put `definition` on opencode, played by a stand-in reporting `version`.
    pub(super) fn use_opencode(project_path: &Path, definition: &str, version: &str) {
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

    pub(super) fn messages(issues: &[Issue], kind: IssueKind) -> Vec<String> {
        issues
            .iter()
            .filter(|i| i.kind() == kind)
            .map(|i| i.message().to_string())
            .collect()
    }
}
