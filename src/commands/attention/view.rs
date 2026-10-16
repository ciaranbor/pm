//! The snapshot's types, and what a feature or a main scope needs from the
//! user, ranked by [`AttentionKind`].

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::state::feature::{FeatureStatus, Progress};
use crate::state::runtime::{Latest, WaitingClass, WaitingKind};

/// Bumped when a field changes meaning or goes away; added fields, kinds
/// and states keep it.
pub const VERSION: u32 = 1;

/// How recent a busy agent's activity must be for its scope to be working.
/// docs/remote-api.md, "Agent states and activity", states it in minutes.
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
    /// Whether this is the machine's home project, where the supervisor
    /// runs (the global config's `home`).
    pub home: bool,
}

/// A scope's team status, session and agents: all the main scope has.
#[derive(Debug, Clone, Serialize)]
pub struct ScopeSnapshot {
    pub session: String,
    pub session_exists: bool,
    pub agents: Vec<AgentSnapshot>,
    /// What the scope needs, by [`main_attention`]'s rule.
    pub attention: Attention,
    /// Wip or blocked: main is never ready.
    pub progress: Progress,
    pub blocked_reason: Option<String>,
    pub blocked_by: Option<String>,
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
    /// Its last reply, on one line; never read for
    /// [`scope_agents`](super::scope_agents), whose callers show none.
    pub latest: Option<Latest>,
    /// The tool call the harness's classifier refused the agent since it
    /// was last prompted ([`Denial`](crate::state::runtime::Denial)),
    /// described.
    #[serde(skip)]
    pub denied: Option<String>,
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
    Asking,
    Blocked,
    Cleanup,
    Ready,
    Dead,
    Unarmed,
    Denied,
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
            Self::Denied => "denied",
            Self::None => "none",
        })
    }
}

pub(super) fn of(kind: AttentionKind, detail: Option<String>, agent: Option<String>) -> Attention {
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
pub(super) fn agent_attention(agent: &AgentSnapshot, kind: AttentionKind) -> Attention {
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

/// The first of `agents` between turns with a denial standing, as denied
/// attention naming it. A busy agent may be working around its denial.
fn denied_in(agents: &[AgentSnapshot]) -> Option<Attention> {
    let (agent, denied) = agents.iter().find_map(|a| {
        let between = matches!(a.state, AgentState::Idle | AgentState::Background);
        Some((a, a.denied.as_ref().filter(|_| between)?))
    })?;
    Some(of(
        AttentionKind::Denied,
        Some(format!("{}: {denied}", agent.name)),
        Some(agent.name.clone()),
    ))
}

/// The attention a feature needs: the first kind that applies, in
/// [`AttentionKind`]'s order. A feature is ready only while none of its
/// agents is busy; its stored progress stays ready meanwhile.
pub fn attention(feature: &FeatureSnapshot) -> Attention {
    if let Some(asking) = agent_in(&feature.agents, AgentState::Asking, AttentionKind::Asking) {
        return asking;
    }
    if feature.progress == Progress::Blocked {
        return of(
            AttentionKind::Blocked,
            feature.blocked_reason.clone(),
            feature.blocked_by.clone(),
        );
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
    if let Some(denied) = denied_in(&feature.agents) {
        return denied;
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

/// The attention a main scope needs: an agent asking, blocked, an agent
/// dead or unarmed, or one denied, in that order. Main has no PR to clean
/// up or merge, and an idle orchestrator is its resting state, not a stall.
pub fn main_attention(main: &ScopeSnapshot) -> Attention {
    if let Some(asking) = agent_in(&main.agents, AgentState::Asking, AttentionKind::Asking) {
        return asking;
    }
    if main.progress == Progress::Blocked {
        return of(
            AttentionKind::Blocked,
            main.blocked_reason.clone(),
            main.blocked_by.clone(),
        );
    }
    [
        (AgentState::Dead, AttentionKind::Dead),
        (AgentState::Unarmed, AttentionKind::Unarmed),
    ]
    .into_iter()
    .find_map(|(state, kind)| agent_in(&main.agents, state, kind))
    .or_else(|| denied_in(&main.agents))
    .unwrap_or_else(|| of(AttentionKind::None, None, None))
}

#[cfg(test)]
impl ScopeSnapshot {
    /// A main scope of `agents` with status `team`, its attention judged.
    pub(crate) fn main_of(
        agents: Vec<AgentSnapshot>,
        team: crate::state::scope::TeamStatus,
    ) -> Self {
        let mut main = Self {
            session: "app/main".into(),
            session_exists: true,
            agents,
            attention: of(AttentionKind::None, None, None),
            progress: team.progress,
            blocked_reason: team.reason().map(str::to_string),
            blocked_by: team.agent().map(str::to_string),
            working: false,
            background_since: None,
            last_activity: None,
        };
        main.attention = main_attention(&main);
        main
    }
}
