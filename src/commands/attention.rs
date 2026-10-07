//! The attention snapshot behind `pm feat status` and `pm status`: every
//! feature, what its agents are doing, and what — if anything — it or a
//! project's main scope needs from the user (README, "Follow what needs
//! you", has the ranking; docs/remote-api.md, "Attention snapshot", the
//! JSON contract). It reads pm state and the tmux server once
//! ([`Windows`]), takes the PR state `pm feat sync` last recorded, and
//! never calls `gh` or a harness, so it is cheap enough to poll.
//!
//! An agent is classified by its window and what it is at
//! ([`classify`]); one busy is refined by that into
//! asking, unarmed or background. One it reads as dead is busy for a few
//! seconds after its spawn: its harness may not have started yet. A scope
//! is working while a busy agent showed activity in the last
//! [`WORKING_SECS`]; a busy agent silent longer reads quiet. Background
//! work is not working: a scope with a background agent dates from its
//! oldest wait, however quiet it is ([`activity`]).

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::error::Result;
use crate::harness::Harness;
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::feature::{FeatureState, FeatureStatus, Progress};
use crate::state::paths;
use crate::state::project::{
    GlobalConfig, HarnessConfig, ProjectConfig, ProjectEntry, resolve_harness_config,
};
use crate::state::runtime::{self, Waiting, WaitingClass, WaitingKind};
use crate::tmux;

use super::feat_status_view::first_line;
use super::hooks_dialog;
use super::running_agents::{AgentAt, Liveness, Windows, classify, starting};

pub mod transition;

/// Bumped when a field changes meaning or goes away; added fields, kinds
/// and states keep it.
pub const VERSION: u32 = 1;

/// How recent a busy agent's activity must be for its scope to be working.
pub const WORKING_SECS: i64 = 20 * 60;

/// A scope quiet for less than this is shown as neither working nor quiet,
/// so the gaps between turns don't flicker.
pub const QUIET_SECS: i64 = 10 * 60;

/// What a scope is doing, as the views show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Working,
    /// Waiting on background work since then.
    Background(DateTime<Utc>),
    /// Quiet since then, for [`QUIET_SECS`] or more.
    Quiet(DateTime<Utc>),
}

/// A scope's activity as of `now`: working, else waiting on background
/// work, else quiet once that is long enough to matter; `None` between
/// turns.
pub fn activity(
    working: bool,
    background_since: Option<DateTime<Utc>>,
    last_activity: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<Activity> {
    if working {
        return Some(Activity::Working);
    }
    if let Some(since) = background_since {
        return Some(Activity::Background(since));
    }
    last_activity
        .filter(|t| (now - *t).num_seconds() >= QUIET_SECS)
        .map(Activity::Quiet)
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub version: u32,
    pub projects: Vec<ProjectSnapshot>,
    /// Every feature of every listed project, most urgent first.
    pub features: Vec<FeatureSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct ProjectSnapshot {
    pub name: String,
    /// Empty when the project's registry entry couldn't be read.
    pub root: String,
    /// Why the project's features are missing from the snapshot.
    pub skipped: Option<String>,
    /// The project's main scope; `None` when the project was skipped.
    pub main: Option<ScopeSnapshot>,
}

/// A scope's session and agents: all the main scope has, with no progress
/// of its own.
#[derive(Debug, Clone, Serialize)]
pub struct ScopeSnapshot {
    pub session: String,
    pub session_exists: bool,
    pub agents: Vec<AgentSnapshot>,
    /// What the scope's agents need, by [`main_attention`]'s rule.
    pub attention: Attention,
    pub working: bool,
    /// When the longest-waiting background agent's wait began.
    pub background_since: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeatureSnapshot {
    pub project: String,
    pub name: String,
    pub attention: Attention,
    pub progress: Progress,
    pub blocked_reason: Option<String>,
    pub blocked_by: Option<String>,
    /// The summary's first line of text.
    pub summary: Option<String>,
    /// The PR-derived status `pm feat sync` last recorded.
    pub lifecycle: FeatureStatus,
    pub pr: Option<String>,
    /// The feature's tmux session.
    pub session: String,
    pub session_exists: bool,
    pub agents: Vec<AgentSnapshot>,
    pub working: bool,
    /// When the longest-waiting background agent's wait began.
    pub background_since: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentSnapshot {
    pub name: String,
    pub state: AgentState,
    pub unread: u32,
    /// The window's tmux target, while it has one.
    pub window: Option<String>,
    /// The agent's pane in that window, by id (`%N`).
    #[serde(skip)]
    pub pane: Option<String>,
    /// What an asking, unarmed or background agent is at.
    pub waiting: Option<WaitingSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitingSnapshot {
    pub kind: WaitingKind,
    pub detail: String,
    /// When the wait began; `None` for a loop that stopped itself.
    pub since: Option<DateTime<Utc>>,
    /// The id of the dialog, when it can be answered remotely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialog: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    /// Waiting for a message, between turns.
    Idle,
    /// Mid-turn.
    Busy,
    /// A dialog waits on the user.
    Asking,
    /// At its prompt, where a message won't wake it.
    Unarmed,
    /// Its turn ended for background work, whose completion wakes it.
    Background,
    /// Active, but its window is gone from a live session or its harness
    /// exited.
    Dead,
    /// Stopped with `pm agent stop`.
    Stopped,
    /// Active, in a feature whose session is closed.
    Closed,
}

impl AgentState {
    fn is_running(self) -> bool {
        matches!(
            self,
            Self::Idle | Self::Busy | Self::Asking | Self::Unarmed | Self::Background
        )
    }
}

impl From<WaitingClass> for AgentState {
    fn from(class: WaitingClass) -> Self {
        match class {
            WaitingClass::Asking => Self::Asking,
            WaitingClass::Unarmed => Self::Unarmed,
            WaitingClass::Background => Self::Background,
        }
    }
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::Idle => "idle",
            Self::Busy => "busy",
            Self::Asking => "asking",
            Self::Unarmed => "unarmed",
            Self::Background => "background",
            Self::Dead => "dead",
            Self::Stopped => "stopped",
            Self::Closed => "closed",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attention {
    pub kind: AttentionKind,
    /// What the kind is about: the blocked reason, the summary line, which
    /// agent died.
    pub detail: Option<String>,
    /// The agent to go to.
    pub agent: Option<String>,
}

/// Most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AttentionKind {
    Blocked,
    Asking,
    Cleanup,
    Ready,
    Dead,
    Unarmed,
    Stalled,
    None,
}

impl std::fmt::Display for AttentionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::Blocked => "blocked",
            Self::Asking => "asking",
            Self::Cleanup => "cleanup",
            Self::Ready => "ready",
            Self::Stalled => "stalled",
            Self::Dead => "dead",
            Self::Unarmed => "unarmed",
            Self::None => "none",
        })
    }
}

fn of(kind: AttentionKind, detail: Option<String>, agent: Option<String>) -> Attention {
    Attention {
        kind,
        detail,
        agent,
    }
}

/// The first of `agents` in `state`, as attention of `kind` naming it.
fn agent_in(agents: &[AgentSnapshot], state: AgentState, kind: AttentionKind) -> Option<Attention> {
    let agent = agents.iter().find(|a| a.state == state)?;
    Some(agent_attention(agent, kind))
}

/// Attention of `kind` naming `agent` and what it is at.
fn agent_attention(agent: &AgentSnapshot, kind: AttentionKind) -> Attention {
    let what = match (&agent.waiting, agent.window.is_some()) {
        (Some(waiting), _) => waiting.detail.as_str(),
        (None, true) => "harness exited",
        (None, false) => "window missing",
    };
    of(
        kind,
        Some(format!("{}: {what}", agent.name)),
        Some(agent.name.clone()),
    )
}

/// The attention a feature needs: the first kind that applies, in
/// [`AttentionKind`]'s order. A feature is ready only while none of its
/// agents is busy; its stored progress stays ready meanwhile.
pub fn attention(feature: &FeatureSnapshot) -> Attention {
    if feature.progress == Progress::Blocked {
        return of(
            AttentionKind::Blocked,
            feature.blocked_reason.clone(),
            feature.blocked_by.clone(),
        );
    }
    if let Some(asking) = agent_in(&feature.agents, AgentState::Asking, AttentionKind::Asking) {
        return asking;
    }
    match feature.lifecycle {
        FeatureStatus::Merged => return of(AttentionKind::Cleanup, Some("PR merged".into()), None),
        FeatureStatus::Stale => return of(AttentionKind::Cleanup, Some("stale".into()), None),
        _ => {}
    }
    if !feature.agents.iter().any(|a| a.state == AgentState::Busy) {
        if feature.progress == Progress::Ready {
            return of(AttentionKind::Ready, feature.summary.clone(), None);
        }
        if feature.lifecycle == FeatureStatus::Approved {
            return of(AttentionKind::Ready, Some("PR approved".into()), None);
        }
    }
    if let Some(dead) = agent_in(&feature.agents, AgentState::Dead, AttentionKind::Dead) {
        return dead;
    }
    if let Some(unarmed) = agent_in(&feature.agents, AgentState::Unarmed, AttentionKind::Unarmed) {
        return unarmed;
    }
    let running: Vec<&AgentSnapshot> = feature
        .agents
        .iter()
        .filter(|a| a.state.is_running())
        .collect();
    if feature.progress == Progress::Wip
        && !running.is_empty()
        && running
            .iter()
            .all(|a| a.state == AgentState::Idle && a.unread == 0)
    {
        return of(AttentionKind::Stalled, None, None);
    }
    of(AttentionKind::None, None, None)
}

/// The attention a main scope needs, which has no progress: an agent
/// asking, dead or unarmed, in that order.
pub fn main_attention(agents: &[AgentSnapshot]) -> Attention {
    [
        (AgentState::Asking, AttentionKind::Asking),
        (AgentState::Dead, AttentionKind::Dead),
        (AgentState::Unarmed, AttentionKind::Unarmed),
    ]
    .into_iter()
    .find_map(|(state, kind)| agent_in(agents, state, kind))
    .unwrap_or_else(|| of(AttentionKind::None, None, None))
}

/// The snapshot of every registered project. A project whose state or
/// registry entry can't be read is listed as skipped, so one broken entry
/// doesn't hide the rest. Nothing is printed: the tmux watcher runs this.
pub fn all(projects_dir: &Path, tmux_server: Option<&str>) -> Result<Snapshot> {
    let windows = Windows::read(tmux_server)?;
    let global = GlobalConfig::load_or_default().harness;
    let mut snapshot = Snapshot {
        version: VERSION,
        projects: Vec::new(),
        features: Vec::new(),
    };
    let registry = ProjectEntry::scan(projects_dir)?;
    for (name, entry) in registry.projects {
        let root = entry.root_path();
        let read = if paths::pm_dir(&root).is_dir() {
            project_features(&root, &windows, &global).map_err(|e| e.to_string())
        } else {
            Err(format!("no pm project at {}", root.display()))
        };
        let (skipped, main) = match read {
            Ok(read) => {
                snapshot.features.extend(read.features);
                (None, Some(read.main))
            }
            Err(e) => (Some(e), None),
        };
        snapshot.projects.push(ProjectSnapshot {
            name,
            root: root.display().to_string(),
            skipped,
            main,
        });
    }
    snapshot
        .projects
        .extend(registry.malformed.into_iter().map(|bad| ProjectSnapshot {
            name: bad.name,
            root: String::new(),
            skipped: Some(format!(
                "registry entry {} unreadable: {}",
                bad.path.display(),
                bad.error
            )),
            main: None,
        }));
    snapshot.projects.sort_by(|a, b| a.name.cmp(&b.name));
    sort(&mut snapshot.features);
    Ok(snapshot)
}

/// The snapshot of the project at `project_root`.
pub fn project(project_root: &Path, tmux_server: Option<&str>) -> Result<Snapshot> {
    let windows = Windows::read(tmux_server)?;
    let global = GlobalConfig::load_or_default().harness;
    let ProjectRead {
        name,
        mut features,
        main,
    } = project_features(project_root, &windows, &global)?;
    sort(&mut features);
    Ok(Snapshot {
        version: VERSION,
        projects: vec![ProjectSnapshot {
            name,
            root: project_root.display().to_string(),
            skipped: None,
            main: Some(main),
        }],
        features,
    })
}

/// The agents of one scope of the project at `project_root`.
pub fn scope_agents(
    project_root: &Path,
    scope: &str,
    tmux_server: Option<&str>,
) -> Result<Vec<AgentSnapshot>> {
    scope_agents_in(project_root, scope, &Windows::read(tmux_server)?)
}

/// [`scope_agents`] from a scan of the windows already taken.
pub fn scope_agents_in(
    project_root: &Path,
    scope: &str,
    windows: &Windows,
) -> Result<Vec<AgentSnapshot>> {
    let project_config = ProjectConfig::load(&paths::pm_dir(project_root))?;
    let config = resolve_harness_config(
        &project_config.harness,
        &GlobalConfig::load_or_default().harness,
    );
    let reader = ScopeReader {
        project_root,
        project: &project_config.project.name,
        windows,
        config: &config,
    };
    Ok(reader.read(scope)?.agents)
}

fn sort(features: &mut [FeatureSnapshot]) {
    features.sort_by(|a, b| {
        (a.attention.kind, &a.project, &a.name).cmp(&(b.attention.kind, &b.project, &b.name))
    });
}

struct ProjectRead {
    /// The project's name, as its sessions are prefixed.
    name: String,
    features: Vec<FeatureSnapshot>,
    main: ScopeSnapshot,
}

fn project_features(
    project_root: &Path,
    windows: &Windows,
    global: &HarnessConfig,
) -> Result<ProjectRead> {
    let project_config = ProjectConfig::load(&paths::pm_dir(project_root))?;
    let config = resolve_harness_config(&project_config.harness, global);
    let project = project_config.project.name;
    let reader = ScopeReader {
        project_root,
        project: &project,
        windows,
        config: &config,
    };
    let features = FeatureState::list(&paths::features_dir(project_root))?
        .into_iter()
        .map(|(name, state)| {
            let ScopeSnapshot {
                session,
                session_exists,
                agents,
                working,
                background_since,
                last_activity,
                ..
            } = reader.read(&name)?;
            let mut feature = FeatureSnapshot {
                project: project.clone(),
                attention: Attention {
                    kind: AttentionKind::None,
                    detail: None,
                    agent: None,
                },
                progress: state.progress,
                blocked_reason: state
                    .blocked_reason
                    .filter(|_| state.progress == Progress::Blocked),
                blocked_by: state
                    .blocked_by
                    .filter(|_| state.progress == Progress::Blocked),
                summary: first_line(project_root, &name),
                lifecycle: state.status,
                pr: Some(state.pr).filter(|pr| !pr.is_empty()),
                session,
                session_exists,
                agents,
                working,
                background_since,
                last_activity,
                name,
            };
            feature.attention = attention(&feature);
            Ok(feature)
        })
        .collect::<Result<_>>()?;
    Ok(ProjectRead {
        main: reader.read("main")?,
        name: project,
        features,
    })
}

/// What a project's scopes are read against.
struct ScopeReader<'a> {
    project_root: &'a Path,
    project: &'a str,
    windows: &'a Windows,
    config: &'a HarnessConfig,
}

impl ScopeReader<'_> {
    fn read(&self, scope: &str) -> Result<ScopeSnapshot> {
        let session = tmux::session_name(self.project, scope);
        let session_exists = self.windows.has_session(&session);
        let registry = AgentRegistry::load(&paths::agents_dir(self.project_root), scope)?;
        let messages_dir = paths::messages_dir(self.project_root);
        let now = Utc::now();
        let mut working = false;
        let mut background_since: Option<DateTime<Utc>> = None;
        let mut last_activity = None;
        let agents: Vec<AgentSnapshot> = registry
            .agents
            .iter()
            .map(|(agent, entry)| {
                let pane = self.windows.find(&session, &entry.window_name);
                let (state, waiting) = match pane {
                    _ if !entry.active => (AgentState::Stopped, None),
                    _ if !session_exists => (AgentState::Closed, None),
                    None => (AgentState::Dead, None),
                    Some(pane) => match classify(
                        AgentAt {
                            project_root: self.project_root,
                            scope,
                            name: agent,
                            harness: entry.harness,
                        },
                        self.windows.processes(pane).as_deref(),
                        self.config,
                    ) {
                        (Liveness::Idle, _) => (AgentState::Idle, None),
                        (Liveness::Busy, at) => self.busy(scope, agent, entry.harness, at, now),
                        (Liveness::Dead, _) if starting(self.project_root, scope, agent, now) => {
                            (AgentState::Busy, None)
                        }
                        (Liveness::Dead, _) => (AgentState::Dead, None),
                    },
                };
                let active = runtime::last_activity(self.project_root, scope, agent);
                let recent = active.is_some_and(|t| (now - t).num_seconds() < WORKING_SECS);
                working |= recent && state == AgentState::Busy;
                if state == AgentState::Background
                    && let Some(since) = waiting.as_ref().and_then(|w| w.since)
                {
                    background_since = Some(background_since.map_or(since, |b| b.min(since)));
                }
                last_activity = last_activity.max(active);
                AgentSnapshot {
                    name: agent.clone(),
                    state,
                    unread: messages::unread_count(&messages_dir, scope, agent),
                    window: pane.map(|p| p.window.clone()),
                    pane: pane.map(|p| p.id.clone()),
                    waiting,
                }
            })
            .collect();
        Ok(ScopeSnapshot {
            session,
            session_exists,
            attention: main_attention(&agents),
            agents,
            working,
            background_since,
            last_activity,
        })
    }

    /// A busy agent, refined by what it is `at` or a stopped loop. A
    /// startup marker counts only once the start has had time to finish.
    fn busy(
        &self,
        scope: &str,
        agent: &str,
        harness: Harness,
        at: Option<Waiting>,
        now: DateTime<Utc>,
    ) -> (AgentState, Option<WaitingSnapshot>) {
        let grace = super::doctor::START_GRACE.as_secs() as i64;
        let waiting = at
            .filter(|w| w.kind != WaitingKind::Startup || (now - w.since).num_seconds() > grace)
            .map(|w| WaitingSnapshot {
                kind: w.kind,
                detail: w.describe(),
                since: Some(w.since),
                dialog: (w.kind.class() == WaitingClass::Asking)
                    .then(|| hooks_dialog::current_for(self.project_root, scope, agent, &w))
                    .flatten()
                    .map(|record| record.dialog.id),
            })
            .or_else(|| {
                let reason = harness.loop_stopped(self.project_root, scope, agent)?;
                Some(WaitingSnapshot {
                    kind: WaitingKind::Tripped,
                    detail: format!("loop stopped: {reason}"),
                    since: None,
                    dialog: None,
                })
            });
        let Some(waiting) = waiting else {
            return (AgentState::Busy, None);
        };
        (waiting.kind.class().into(), Some(waiting))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::running_agents::STARTING_SECS;
    use crate::commands::{feat_new, feat_status::feat_status, init};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn feature(progress: Progress, lifecycle: FeatureStatus) -> FeatureSnapshot {
        FeatureSnapshot {
            project: "app".into(),
            name: "login".into(),
            attention: attention_none(),
            progress,
            blocked_reason: None,
            blocked_by: None,
            summary: Some("Adds login".into()),
            lifecycle,
            pr: None,
            session: "app/login".into(),
            session_exists: true,
            agents: Vec::new(),
            working: false,
            background_since: None,
            last_activity: None,
        }
    }

    fn attention_none() -> Attention {
        Attention {
            kind: AttentionKind::None,
            detail: None,
            agent: None,
        }
    }

    fn agent(name: &str, state: AgentState, unread: u32) -> AgentSnapshot {
        AgentSnapshot {
            name: name.into(),
            state,
            unread,
            window: (state != AgentState::Dead).then(|| format!("app/login:{name}")),
            pane: None,
            waiting: None,
        }
    }

    fn waiting(name: &str, state: AgentState, kind: WaitingKind, detail: &str) -> AgentSnapshot {
        AgentSnapshot {
            waiting: Some(WaitingSnapshot {
                kind,
                detail: detail.into(),
                since: None,
                dialog: None,
            }),
            ..agent(name, state, 0)
        }
    }

    fn kind(feature: &FeatureSnapshot) -> AttentionKind {
        attention(feature).kind
    }

    #[test]
    fn blocked_names_the_reason_and_the_agent_that_set_it() {
        let mut f = feature(Progress::Blocked, FeatureStatus::Merged);
        f.blocked_reason = Some("which DB?".into());
        f.blocked_by = Some("implementer".into());
        f.agents = vec![agent("implementer", AgentState::Dead, 0)];

        assert_eq!(
            attention(&f),
            Attention {
                kind: AttentionKind::Blocked,
                detail: Some("which DB?".into()),
                agent: Some("implementer".into()),
            }
        );
    }

    #[test]
    fn a_merged_or_stale_feature_needs_deleting_whatever_its_progress() {
        for lifecycle in [FeatureStatus::Merged, FeatureStatus::Stale] {
            let mut f = feature(Progress::Ready, lifecycle);
            f.agents = vec![agent("implementer", AgentState::Idle, 0)];
            assert_eq!(kind(&f), AttentionKind::Cleanup, "{lifecycle}");
        }
    }

    #[test]
    fn ready_progress_or_an_approved_pr_can_merge() {
        let mut f = feature(Progress::Ready, FeatureStatus::Review);
        f.agents = vec![agent("implementer", AgentState::Dead, 0)];
        assert_eq!(attention(&f).kind, AttentionKind::Ready);
        assert_eq!(attention(&f).detail.as_deref(), Some("Adds login"));

        let mut f = feature(Progress::Wip, FeatureStatus::Approved);
        f.agents = vec![agent("implementer", AgentState::Idle, 0)];
        assert_eq!(attention(&f).kind, AttentionKind::Ready);
        assert_eq!(attention(&f).detail.as_deref(), Some("PR approved"));
    }

    #[test]
    fn a_feature_is_ready_only_while_none_of_its_agents_is_busy() {
        for (progress, lifecycle) in [
            (Progress::Ready, FeatureStatus::Review),
            (Progress::Wip, FeatureStatus::Approved),
        ] {
            let mut f = feature(progress, lifecycle);
            f.agents = vec![
                agent("implementer", AgentState::Idle, 0),
                agent("reviewer", AgentState::Busy, 0),
            ];
            assert_eq!(kind(&f), AttentionKind::None, "{progress} {lifecycle}");

            f.agents[1] = waiting(
                "reviewer",
                AgentState::Background,
                WaitingKind::Background,
                "CI watch",
            );
            assert_eq!(kind(&f), AttentionKind::Ready, "background work isn't busy");
        }
    }

    #[test]
    fn a_wip_team_all_idle_with_nothing_to_read_is_stalled() {
        let mut f = feature(Progress::Wip, FeatureStatus::Wip);
        f.agents = vec![
            agent("implementer", AgentState::Idle, 0),
            agent("reviewer", AgentState::Idle, 0),
            agent("qa", AgentState::Stopped, 3),
        ];
        assert_eq!(kind(&f), AttentionKind::Stalled);

        f.agents[1].unread = 1;
        assert_eq!(kind(&f), AttentionKind::None, "a message will wake it");
        f.agents[1] = agent("reviewer", AgentState::Busy, 0);
        assert_eq!(kind(&f), AttentionKind::None, "one is working");
    }

    #[test]
    fn a_dead_agent_outranks_an_idle_team() {
        let mut f = feature(Progress::Wip, FeatureStatus::Wip);
        f.agents = vec![
            agent("implementer", AgentState::Idle, 0),
            agent("reviewer", AgentState::Dead, 0),
        ];
        assert_eq!(kind(&f), AttentionKind::Dead);
    }

    #[test]
    fn activity_is_work_then_background_work_then_a_quiet_spell_long_enough_to_matter() {
        let now = Utc::now();
        let ago = |minutes| Some(now - chrono::Duration::minutes(minutes));
        let background = ago(1500);
        assert_eq!(
            activity(true, background, ago(1), now),
            Some(Activity::Working)
        );
        assert_eq!(
            activity(false, background, ago(185), now),
            Some(Activity::Background(background.unwrap()))
        );
        assert_eq!(activity(false, None, ago(9), now), None, "between turns");
        assert_eq!(
            activity(false, None, ago(185), now),
            Some(Activity::Quiet(ago(185).unwrap()))
        );
        assert_eq!(activity(false, None, None, now), None);
    }

    #[test]
    fn the_json_keeps_the_documented_shape() {
        let mut f = feature(Progress::Blocked, FeatureStatus::Wip);
        f.blocked_reason = Some("which DB?".into());
        f.blocked_by = Some("implementer".into());
        f.agents = vec![agent("implementer", AgentState::Idle, 0)];
        f.attention = attention(&f);
        let snapshot = Snapshot {
            version: VERSION,
            projects: vec![ProjectSnapshot {
                name: "app".into(),
                root: "/src/app".into(),
                skipped: None,
                main: Some(ScopeSnapshot {
                    session: "app/main".into(),
                    session_exists: true,
                    agents: vec![AgentSnapshot {
                        window: Some("app/main:1".into()),
                        pane: None,
                        waiting: Some(WaitingSnapshot {
                            kind: WaitingKind::Plan,
                            detail: "plan approval".into(),
                            since: Some("2026-10-02T09:25:00Z".parse().unwrap()),
                            dialog: None,
                        }),
                        ..agent("main", AgentState::Asking, 0)
                    }],
                    attention: Attention {
                        kind: AttentionKind::Asking,
                        detail: Some("main: plan approval".into()),
                        agent: Some("main".into()),
                    },
                    working: false,
                    background_since: Some("2026-10-01T08:00:00Z".parse().unwrap()),
                    last_activity: Some("2026-10-02T09:30:00Z".parse().unwrap()),
                }),
            }],
            features: vec![f],
        };

        assert_eq!(
            serde_json::to_value(&snapshot).unwrap(),
            serde_json::json!({
                "version": 1,
                "projects": [{
                    "name": "app",
                    "root": "/src/app",
                    "skipped": null,
                    "main": {
                        "session": "app/main",
                        "session_exists": true,
                        "agents": [{
                            "name": "main",
                            "state": "asking",
                            "unread": 0,
                            "window": "app/main:1",
                            "waiting": {
                                "kind": "plan",
                                "detail": "plan approval",
                                "since": "2026-10-02T09:25:00Z"
                            }
                        }],
                        "attention": {
                            "kind": "asking",
                            "detail": "main: plan approval",
                            "agent": "main"
                        },
                        "working": false,
                        "background_since": "2026-10-01T08:00:00Z",
                        "last_activity": "2026-10-02T09:30:00Z"
                    }
                }],
                "features": [{
                    "project": "app",
                    "name": "login",
                    "attention": { "kind": "blocked", "detail": "which DB?", "agent": "implementer" },
                    "progress": "blocked",
                    "blocked_reason": "which DB?",
                    "blocked_by": "implementer",
                    "summary": "Adds login",
                    "lifecycle": "wip",
                    "pr": null,
                    "session": "app/login",
                    "session_exists": true,
                    "agents": [{
                        "name": "implementer",
                        "state": "idle",
                        "unread": 0,
                        "window": "app/login:implementer",
                        "waiting": null
                    }],
                    "working": false,
                    "background_since": null,
                    "last_activity": null
                }]
            })
        );
        let kinds: Vec<serde_json::Value> = [
            AttentionKind::Cleanup,
            AttentionKind::Ready,
            AttentionKind::Dead,
            AttentionKind::Unarmed,
            AttentionKind::Stalled,
            AttentionKind::None,
        ]
        .iter()
        .map(|k| serde_json::to_value(k).unwrap())
        .collect();
        assert_eq!(
            kinds,
            ["cleanup", "ready", "dead", "unarmed", "stalled", "none"]
        );
        let states: Vec<serde_json::Value> = [
            AgentState::Busy,
            AgentState::Unarmed,
            AgentState::Background,
            AgentState::Dead,
            AgentState::Stopped,
            AgentState::Closed,
        ]
        .iter()
        .map(|s| serde_json::to_value(s).unwrap())
        .collect();
        assert_eq!(
            states,
            ["busy", "unarmed", "background", "dead", "stopped", "closed"]
        );
        let waiting: Vec<serde_json::Value> = [
            WaitingKind::Question,
            WaitingKind::Permission,
            WaitingKind::Dialog,
            WaitingKind::Startup,
            WaitingKind::Interrupted,
            WaitingKind::HookEnded,
            WaitingKind::Error,
            WaitingKind::Prompt,
            WaitingKind::Tripped,
            WaitingKind::Background,
        ]
        .iter()
        .map(|k| serde_json::to_value(k).unwrap())
        .collect();
        assert_eq!(
            waiting,
            [
                "question",
                "permission",
                "dialog",
                "startup",
                "interrupted",
                "hook-ended",
                "error",
                "prompt",
                "tripped",
                "background"
            ]
        );
    }

    #[test]
    fn an_asking_agent_ranks_after_blocked_and_before_everything_else() {
        let mut f = feature(Progress::Wip, FeatureStatus::Merged);
        f.agents = vec![
            agent("implementer", AgentState::Dead, 0),
            waiting(
                "reviewer",
                AgentState::Asking,
                WaitingKind::Question,
                "Which DB?",
            ),
        ];
        assert_eq!(
            attention(&f),
            Attention {
                kind: AttentionKind::Asking,
                detail: Some("reviewer: Which DB?".into()),
                agent: Some("reviewer".into()),
            }
        );
        f.progress = Progress::Blocked;
        assert_eq!(kind(&f), AttentionKind::Blocked);
    }

    #[test]
    fn an_unarmed_agent_ranks_after_a_dead_one() {
        let mut f = feature(Progress::Wip, FeatureStatus::Wip);
        f.agents = vec![waiting(
            "implementer",
            AgentState::Unarmed,
            WaitingKind::Interrupted,
            "interrupted",
        )];
        assert_eq!(
            attention(&f).detail.as_deref(),
            Some("implementer: interrupted")
        );
        assert_eq!(kind(&f), AttentionKind::Unarmed);
        f.agents.push(agent("reviewer", AgentState::Dead, 0));
        assert_eq!(kind(&f), AttentionKind::Dead);
    }

    #[test]
    fn a_waiting_agent_keeps_its_team_from_stalling() {
        for (state, waiting_kind, kind_) in [
            (
                AgentState::Asking,
                WaitingKind::Question,
                AttentionKind::Asking,
            ),
            (
                AgentState::Unarmed,
                WaitingKind::Interrupted,
                AttentionKind::Unarmed,
            ),
            (
                AgentState::Background,
                WaitingKind::Background,
                AttentionKind::None,
            ),
        ] {
            let mut f = feature(Progress::Wip, FeatureStatus::Wip);
            f.agents = vec![
                agent("implementer", AgentState::Idle, 0),
                waiting("reviewer", state, waiting_kind, "x"),
            ];
            assert_eq!(kind(&f), kind_, "{state}");
        }
    }

    #[test]
    fn main_needs_an_asking_then_a_dead_then_an_unarmed_agent() {
        let unarmed = waiting("a", AgentState::Unarmed, WaitingKind::Error, "API error");
        let dead = agent("b", AgentState::Dead, 0);
        let asking = waiting("c", AgentState::Asking, WaitingKind::Plan, "plan approval");
        let idle = agent("main", AgentState::Idle, 0);
        let kind_of = |agents: Vec<AgentSnapshot>| main_attention(&agents).kind;

        assert_eq!(kind_of(vec![idle.clone()]), AttentionKind::None);
        assert_eq!(
            kind_of(vec![idle.clone(), unarmed.clone()]),
            AttentionKind::Unarmed
        );
        assert_eq!(
            kind_of(vec![unarmed.clone(), dead.clone()]),
            AttentionKind::Dead
        );
        assert_eq!(
            main_attention(&[unarmed, dead, asking]),
            Attention {
                kind: AttentionKind::Asking,
                detail: Some("c: plan approval".into()),
                agent: Some("c".into()),
            }
        );
    }

    #[test]
    fn a_marker_refines_only_an_agent_its_window_reads_as_busy() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project, &session, "login", "asking");
        server.spawn_fake_agent(&project, &session, "login", "starting");
        server.spawn_idle_fake_agent(&project, &session, "login", "idle");
        server.spawn_dead_fake_agent(&project, &session, "login", "dead");
        server.spawn_dead_fake_agent(&project, &session, "login", "spawned");
        server.spawn_dead_fake_agent(&project, &session, "login", "never-started");
        server.spawn_dead_fake_agent(&project, &session, "login", "in-rc");
        server.spawn_dead_fake_agent(&project, &session, "login", "stuck-in-rc");
        let mark = |agent: &str, kind: WaitingKind| {
            let waiting = runtime::Waiting::now(kind, Some("Which DB?".into()));
            runtime::write_waiting(&project, "login", agent, &waiting).unwrap();
        };
        mark("asking", WaitingKind::Question);
        mark("starting", WaitingKind::Startup);
        mark("dead", WaitingKind::Question);
        mark("spawned", WaitingKind::Startup);
        let spawned_ago = |agent: &str, secs: i64| {
            let mut stale = runtime::Waiting::now(WaitingKind::Startup, None);
            stale.since -= chrono::Duration::seconds(secs);
            runtime::write_waiting(&project, "login", agent, &stale).unwrap();
        };
        let past_starting = STARTING_SECS + 1;
        spawned_ago("never-started", past_starting);
        let launched = runtime::reset_launched(&project, "login", "never-started").unwrap();
        std::fs::File::create(&launched)
            .unwrap()
            .set_modified(
                std::time::SystemTime::now() - std::time::Duration::from_secs(past_starting as u64),
            )
            .unwrap();
        spawned_ago("in-rc", past_starting);
        let past_launch = super::super::launch_check::START_WITHIN.as_secs() as i64 + 1;
        spawned_ago("stuck-in-rc", past_launch);
        runtime::touch_activity(&project, "login", "asking").unwrap();

        let login = &super::project(&project, server.name()).unwrap().features[0];

        let states: Vec<(&str, AgentState, Option<&str>)> = login
            .agents
            .iter()
            .map(|a| {
                let detail = a.waiting.as_ref().map(|w| w.detail.as_str());
                (a.name.as_str(), a.state, detail)
            })
            .collect();
        assert_eq!(
            states,
            [
                ("asking", AgentState::Asking, Some("Which DB?")),
                ("dead", AgentState::Dead, None),
                ("idle", AgentState::Idle, None),
                ("in-rc", AgentState::Busy, None),
                ("never-started", AgentState::Dead, None),
                ("spawned", AgentState::Busy, None),
                ("starting", AgentState::Busy, None),
                ("stuck-in-rc", AgentState::Dead, None),
            ]
        );
        assert_eq!(login.attention.kind, AttentionKind::Asking);
        assert!(!login.working, "its only busy agent was never active");
        assert_eq!(login.agents[0].waiting.as_ref().unwrap().dialog, None);

        // A dialog whose hook (this process) waits names it.
        let payload = serde_json::json!({
            "hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
            "tool_input": {"questions": [{"question": "Which DB?", "options": []}]}
        });
        let (dialog, reply_context) = Harness::ClaudeCode.dialog(&payload).unwrap();
        let id = dialog.id.clone();
        let record = runtime::DialogRecord {
            dialog,
            pid: Some(std::process::id()),
            reply_context,
        };
        runtime::write_dialog(&project, "login", "asking", &record).unwrap();
        let login = &super::project(&project, server.name()).unwrap().features[0];
        assert_eq!(login.agents[0].waiting.as_ref().unwrap().dialog, Some(id));

        let at = |minutes| Utc::now() - chrono::Duration::minutes(minutes);
        runtime::set_activity(&project, "login", "starting", at(1));
        let login = &super::project(&project, server.name()).unwrap().features[0];
        assert!(login.working);

        runtime::set_activity(&project, "login", "starting", at(30));
        runtime::set_activity(&project, "login", "asking", at(25));
        let login = &super::project(&project, server.name()).unwrap().features[0];
        assert!(!login.working, "a busy agent silent for 30 minutes");
        let quiet = Utc::now() - login.last_activity.unwrap();
        assert_eq!(quiet.num_minutes(), 25, "the latest of its agents");
    }

    #[test]
    fn background_work_is_not_working_and_dates_from_the_oldest_wait() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        let started = |agent: &str, hours| {
            let target = server.spawn_fake_agent(&project, &session, "login", agent);
            // The harness itself stands for its background work's waiter.
            let processes = tmux::pane_processes(server.name(), &target).unwrap();
            let waiter = processes.last().unwrap().pid;
            runtime::take_waiter(&project, "login", agent, waiter, None).unwrap();
            let mut waiting = runtime::Waiting::now(WaitingKind::Background, None);
            waiting.since -= chrono::Duration::hours(hours);
            runtime::write_waiting(&project, "login", agent, &waiting).unwrap();
            runtime::touch_activity(&project, "login", agent).unwrap();
            waiting.since
        };
        let oldest = started("build", 30);
        started("eval", 2);

        let login = &super::project(&project, server.name()).unwrap().features[0];
        assert!(!login.working);
        assert_eq!(login.background_since, Some(oldest));
        assert_eq!(login.attention.kind, AttentionKind::None, "not stalled");

        server.spawn_fake_agent(&project, &session, "login", "implementer");
        runtime::touch_activity(&project, "login", "implementer").unwrap();
        let login = &super::project(&project, server.name()).unwrap().features[0];
        assert!(login.working, "a busy agent is");
    }

    #[test]
    fn a_team_with_no_running_agent_is_not_stalled() {
        let mut f = feature(Progress::Wip, FeatureStatus::Wip);
        assert_eq!(kind(&f), AttentionKind::None);
        f.agents = vec![agent("implementer", AgentState::Closed, 0)];
        assert_eq!(kind(&f), AttentionKind::None);
    }

    #[test]
    fn a_dead_agent_says_whether_its_window_or_its_harness_is_gone() {
        let mut f = feature(Progress::Wip, FeatureStatus::Wip);
        f.agents = vec![
            agent("implementer", AgentState::Busy, 0),
            agent("reviewer", AgentState::Dead, 0),
        ];
        assert_eq!(
            attention(&f),
            Attention {
                kind: AttentionKind::Dead,
                detail: Some("reviewer: window missing".into()),
                agent: Some("reviewer".into()),
            }
        );
        f.agents[1].window = Some("app/login:2".into());
        assert_eq!(
            attention(&f).detail.as_deref(),
            Some("reviewer: harness exited")
        );
    }

    #[test]
    fn the_project_snapshot_reads_each_agents_window() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project, &session, "login", "implementer");
        server.spawn_dead_fake_agent(&project, &session, "login", "reviewer");
        server.spawn_fake_agent(&project, &session, "login", "qa");
        messages::send(&paths::messages_dir(&project), "login", "qa", "user", "hi").unwrap();

        let snapshot = super::project(&project, server.name()).unwrap();

        let login = &snapshot.features[0];
        assert!(login.session_exists);
        let states: Vec<(&str, AgentState, u32)> = login
            .agents
            .iter()
            .map(|a| (a.name.as_str(), a.state, a.unread))
            .collect();
        assert_eq!(
            states,
            [
                ("implementer", AgentState::Idle, 0),
                ("qa", AgentState::Busy, 1),
                ("reviewer", AgentState::Dead, 0),
            ]
        );
        assert_eq!(login.attention.kind, AttentionKind::Dead);
        assert_eq!(
            login.attention.detail.as_deref(),
            Some("reviewer: harness exited")
        );
    }

    #[test]
    fn an_agent_without_a_window_is_dead_only_while_its_session_is_open() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        let window = server.spawn_idle_fake_agent(&project, &session, "login", "implementer");
        tmux::kill_window(server.name(), &window).unwrap();

        let login = &super::project(&project, server.name()).unwrap().features[0];
        assert_eq!(login.agents[0].state, AgentState::Dead);
        assert_eq!(login.agents[0].window, None);
        assert_eq!(login.attention.kind, AttentionKind::Dead);

        tmux::kill_session(server.name(), &session).unwrap();
        let login = &super::project(&project, server.name()).unwrap().features[0];
        assert!(!login.session_exists);
        assert_eq!(login.agents[0].state, AgentState::Closed);
        assert_eq!(login.attention.kind, AttentionKind::None);
    }

    #[test]
    fn every_project_is_snapshotted_most_urgent_first_and_an_unreadable_one_skipped() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let projects_dir = dir.path().join("registry");
        let mut roots = Vec::new();
        for (project, feature) in [("alpha", "login"), ("beta", "search")] {
            let root = dir.path().join(server.scope(project));
            init::init(&root, &projects_dir, None, server.name()).unwrap();
            feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
                &root,
                &projects_dir,
                feature,
                server.name(),
            ))
            .unwrap();
            roots.push(root);
        }
        feat_status(&roots[1], "search", Progress::Blocked, Some("why?"), None).unwrap();
        let gone = dir.path().join(server.scope("gone"));
        init::init(&gone, &projects_dir, None, server.name()).unwrap();
        std::fs::remove_dir_all(&gone).unwrap();
        let broken = projects_dir.join(format!("{}.toml", server.scope("broken")));
        std::fs::write(&broken, "root = \"/x\"\nmain_branch = [").unwrap();

        let snapshot = all(&projects_dir, server.name()).unwrap();

        let features: Vec<(String, &str, AttentionKind)> = snapshot
            .features
            .iter()
            .map(|f| (f.project.clone(), f.name.as_str(), f.attention.kind))
            .collect();
        assert_eq!(
            features,
            [
                (server.scope("beta"), "search", AttentionKind::Blocked),
                (server.scope("alpha"), "login", AttentionKind::None),
            ]
        );
        let skipped: Vec<(&str, Option<&str>)> = snapshot
            .projects
            .iter()
            .map(|p| (p.name.as_str(), p.skipped.as_deref()))
            .collect();
        assert_eq!(skipped.len(), 4);
        assert!(skipped.contains(&(server.scope("alpha").as_str(), None)));
        assert!(
            skipped
                .iter()
                .any(|(name, why)| *name == server.scope("gone")
                    && why.is_some_and(|w| w.starts_with("no pm project at")))
        );

        let lines =
            crate::commands::feat_status_view::all(&projects_dir, false, server.name()).unwrap();
        let words: Vec<Vec<&str>> = lines
            .iter()
            .map(|l| l.split_whitespace().collect())
            .collect();
        assert_eq!(
            words[..2],
            [
                vec![
                    format!("{}/search", server.scope("beta")).as_str(),
                    "blocked",
                    "why?"
                ],
                vec![format!("{}/login", server.scope("alpha")).as_str(), "wip"],
            ]
        );
        assert!(
            lines[2].starts_with(&format!(
                "{}: skipped (registry entry {} unreadable: line 2: ",
                server.scope("broken"),
                broken.display()
            )),
            "{}",
            lines[2]
        );
        assert_eq!(
            lines[3],
            format!(
                "{}: skipped (no pm project at {})",
                server.scope("gone"),
                gone.display()
            )
        );
        assert_eq!(lines.len(), 4);
    }
}
