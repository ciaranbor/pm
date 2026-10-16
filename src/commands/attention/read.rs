//! Reading the snapshot. It reads pm state and the tmux server once
//! ([`Windows`]), takes the PR state `pm feat sync` last recorded, and
//! never calls `gh` or a harness, so it is cheap enough to poll. Each
//! agent's conversation, read for its latest line, it rereads only once that
//! has changed (`attention/latest.rs`).
//!
//! An agent is classified by its window and what it is at
//! ([`classify`]); one busy is refined by that into
//! asking, unarmed or background. One it reads as dead is busy for a few
//! seconds after its spawn: its harness may not have started yet. A scope
//! is working while a busy agent showed activity in the last
//! [`WORKING_SECS`]; a busy agent silent longer reads quiet. Background
//! work is not working: a scope with a background agent dates from its
//! oldest wait, however quiet it is ([`activity`](super::activity)).

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::commands::feat_status_view::first_line;
use crate::commands::running_agents::{AgentAt, Liveness, Windows, classify, starting};
use crate::commands::{doctor, hooks_dialog, onboard};
use crate::error::Result;
use crate::harness::Harness;
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::{
    GlobalConfig, HarnessConfig, ProjectConfig, ProjectEntry, resolve_harness_config,
};
use crate::state::runtime::{self, Latest, Waiting, WaitingClass, WaitingKind};
use crate::state::scope::{self, TeamStatus};
use crate::tmux;

use super::latest;
use super::view::{
    AgentSnapshot, AgentState, Attention, AttentionKind, FeatureSnapshot, ProjectSnapshot,
    ScopeSnapshot, Snapshot, VERSION, WORKING_SECS, WaitingSnapshot, attention, main_attention, of,
};

/// The snapshot of every registered project on this machine
/// ([`Presence`](crate::state::project::Presence)); another is left out, as
/// nothing here can act on it. A project whose state or registry entry
/// can't be read is listed as skipped, so one broken entry doesn't hide the
/// rest. Nothing is printed: the tmux watcher runs this.
pub fn all(projects_dir: &Path, tmux_server: Option<&str>) -> Result<Snapshot> {
    let windows = Windows::read(tmux_server)?;
    let config = GlobalConfig::load_or_default();
    let global = config.harness.clone();
    let mut snapshot = Snapshot {
        version: VERSION,
        projects: Vec::new(),
        features: Vec::new(),
    };
    let registry = ProjectEntry::scan(projects_dir)?;
    for (name, entry) in registry.projects {
        if !entry.presence().is_here() {
            continue;
        }
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
            home: onboard::is_home(&config, &name, &root),
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
            home: false,
        }));
    snapshot.projects.sort_by(|a, b| a.name.cmp(&b.name));
    sort(&mut snapshot.features);
    Ok(snapshot)
}

/// The snapshot of the project at `project_root`.
pub fn project(project_root: &Path, tmux_server: Option<&str>) -> Result<Snapshot> {
    let windows = Windows::read(tmux_server)?;
    let config = GlobalConfig::load_or_default();
    let global = config.harness.clone();
    let ProjectRead {
        name,
        mut features,
        main,
    } = project_features(project_root, &windows, &global)?;
    sort(&mut features);
    Ok(Snapshot {
        version: VERSION,
        projects: vec![ProjectSnapshot {
            home: onboard::is_home(&config, &name, project_root),
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
        latest: false,
    };
    Ok(reader.read(scope, TeamStatus::default())?.agents)
}

/// The latest line of each agent of `scope` that has one, in the
/// registry's order.
pub fn scope_latest(project_root: &Path, scope: &str) -> Result<Vec<(String, Latest)>> {
    let registry = AgentRegistry::load(&paths::agents_dir(project_root), scope)?;
    Ok(registry
        .agents
        .iter()
        .filter_map(|(name, entry)| {
            Some((
                name.clone(),
                latest::latest(project_root, scope, name, entry)?,
            ))
        })
        .collect())
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
        latest: true,
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
            } = reader.read(&name, state.team.clone())?;
            let mut feature = FeatureSnapshot {
                project: project.clone(),
                attention: Attention {
                    kind: AttentionKind::None,
                    detail: None,
                    agent: None,
                },
                progress: state.team.progress,
                blocked_reason: state.team.reason().map(str::to_string),
                blocked_by: state.team.agent().map(str::to_string),
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
        main: reader.read(scope::MAIN, TeamStatus::load(project_root, scope::MAIN)?)?,
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
    /// Whether each agent's latest line is read.
    latest: bool,
}

impl ScopeReader<'_> {
    /// `scope`, whose status is `team`.
    fn read(&self, scope: &str, team: TeamStatus) -> Result<ScopeSnapshot> {
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
                let denied =
                    runtime::read_denial(self.project_root, scope, agent).map(|d| d.describe());
                AgentSnapshot {
                    name: agent.clone(),
                    state,
                    unread: messages::unread_count(&messages_dir, scope, agent),
                    window: pane.map(|p| p.window.clone()),
                    pane: pane.map(|p| p.id.clone()),
                    waiting,
                    latest: self
                        .latest
                        .then(|| latest::latest(self.project_root, scope, agent, entry))
                        .flatten(),
                    denied,
                }
            })
            .collect();
        let mut snapshot = ScopeSnapshot {
            session,
            session_exists,
            attention: of(AttentionKind::None, None, None),
            progress: team.progress,
            blocked_reason: team.reason().map(str::to_string),
            blocked_by: team.agent().map(str::to_string),
            agents,
            working,
            background_since,
            last_activity,
        };
        snapshot.attention = main_attention(&snapshot);
        Ok(snapshot)
    }

    /// A busy agent, refined by what it is `at` or a stopped loop. A
    /// startup marker counts only once the start has had time to finish.
    /// An asking marker with no open dialog counts as busy once one of the
    /// agent's dialogs has been answered since the marker was set: the
    /// dialog's hook closes the dialog before it re-marks the agent.
    fn busy(
        &self,
        scope: &str,
        agent: &str,
        harness: Harness,
        at: Option<Waiting>,
        now: DateTime<Utc>,
    ) -> (AgentState, Option<WaitingSnapshot>) {
        let grace = doctor::START_GRACE.as_secs() as i64;
        let waiting = at
            .filter(|w| w.kind != WaitingKind::Startup || (now - w.since).num_seconds() > grace)
            .map(|w| {
                let dialog = (w.kind.class() == WaitingClass::Asking)
                    .then(|| hooks_dialog::current_for(self.project_root, scope, agent, &w))
                    .flatten();
                (w, dialog)
            })
            .filter(|(w, dialog)| {
                dialog.is_some()
                    || w.kind.class() != WaitingClass::Asking
                    || !runtime::dialog_answered_since(self.project_root, scope, agent, w.since)
            })
            .map(|(w, dialog)| WaitingSnapshot {
                kind: w.kind,
                detail: w.describe(),
                since: Some(w.since),
                dialog: dialog.map(|record| record.dialog.id),
            });
        // A stopped loop overrides any marker but an asking one: whatever
        // the agent is at, nothing wakes it.
        let asking = waiting
            .as_ref()
            .is_some_and(|w| w.kind.class() == WaitingClass::Asking);
        let stopped = (!asking)
            .then(|| harness.loop_stopped(self.project_root, scope, agent))
            .flatten()
            .map(|reason| WaitingSnapshot {
                kind: WaitingKind::Tripped,
                detail: format!("loop stopped: {reason}"),
                since: None,
                dialog: None,
            });
        let waiting = stopped.or(waiting);
        let Some(waiting) = waiting else {
            return (AgentState::Busy, None);
        };
        (waiting.kind.class().into(), Some(waiting))
    }
}
