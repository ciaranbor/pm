//! The attention snapshot behind `pm feat status` and `pm status`: every
//! feature, what its agents are doing, and what — if anything — it needs
//! from the user (README, "Attention view", has the ranking and the JSON
//! contract). It reads pm state and the tmux server once ([`Windows`]),
//! takes the PR state `pm feat sync` last recorded, and never calls `gh` or
//! a harness, so it is cheap enough to poll.

use std::path::Path;

use serde::Serialize;

use crate::error::Result;
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::feature::{FeatureState, FeatureStatus, Progress};
use crate::state::paths;
use crate::state::project::{
    GlobalConfig, HarnessConfig, ProjectConfig, ProjectEntry, resolve_harness_config,
};
use crate::tmux;

use super::feat_status_view::first_line;
use super::running_agents::{Liveness, Windows, liveness};

/// Bumped when a field changes meaning or goes away; added fields keep it.
pub const VERSION: u32 = 1;

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
    pub root: String,
    /// Why the project's features are missing from the snapshot.
    pub skipped: Option<String>,
    /// The project's main scope; `None` when the project was skipped.
    pub main: Option<ScopeSnapshot>,
}

/// A scope's session and agents: all the main scope has, with no progress
/// or attention of its own.
#[derive(Debug, Clone, Serialize)]
pub struct ScopeSnapshot {
    pub session: String,
    pub session_exists: bool,
    pub agents: Vec<AgentSnapshot>,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentSnapshot {
    pub name: String,
    pub state: AgentState,
    pub unread: u32,
    /// The window's tmux target, while it has one.
    pub window: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    /// Waiting for a message, between turns.
    Idle,
    /// Mid-turn, or running background work.
    Busy,
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
        matches!(self, Self::Idle | Self::Busy)
    }
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::Idle => "idle",
            Self::Busy => "busy",
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
    Cleanup,
    Ready,
    Dead,
    Stalled,
    None,
}

impl std::fmt::Display for AttentionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::Blocked => "blocked",
            Self::Cleanup => "cleanup",
            Self::Ready => "ready",
            Self::Stalled => "stalled",
            Self::Dead => "dead",
            Self::None => "none",
        })
    }
}

/// The attention a feature needs: the first kind that applies, in
/// [`AttentionKind`]'s order.
pub fn attention(feature: &FeatureSnapshot) -> Attention {
    let of = |kind, detail: Option<String>, agent: Option<String>| Attention {
        kind,
        detail,
        agent,
    };
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
    if feature.progress == Progress::Ready {
        return of(AttentionKind::Ready, feature.summary.clone(), None);
    }
    if feature.lifecycle == FeatureStatus::Approved {
        return of(AttentionKind::Ready, Some("PR approved".into()), None);
    }
    if let Some(dead) = feature.agents.iter().find(|a| a.state == AgentState::Dead) {
        let why = if dead.window.is_some() {
            "harness exited"
        } else {
            "window missing"
        };
        return of(
            AttentionKind::Dead,
            Some(format!("{}: {why}", dead.name)),
            Some(dead.name.clone()),
        );
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

/// The snapshot of every registered project. A project whose state can't be
/// read is listed as skipped, so one broken entry doesn't hide the rest.
pub fn all(projects_dir: &Path, tmux_server: Option<&str>) -> Result<Snapshot> {
    let windows = Windows::read(tmux_server)?;
    let global = GlobalConfig::load_or_default().harness;
    let mut snapshot = Snapshot {
        version: VERSION,
        projects: Vec::new(),
        features: Vec::new(),
    };
    for (name, entry) in ProjectEntry::list(projects_dir)? {
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
        let agents = registry
            .agents
            .iter()
            .map(|(agent, entry)| {
                let pane = self.windows.find(&session, &entry.window_name);
                let state = match pane {
                    _ if !entry.active => AgentState::Stopped,
                    _ if !session_exists => AgentState::Closed,
                    None => AgentState::Dead,
                    Some(pane) => match liveness(
                        self.windows.processes(pane).as_deref(),
                        entry.harness,
                        self.config,
                    ) {
                        Liveness::Idle => AgentState::Idle,
                        Liveness::Busy => AgentState::Busy,
                        Liveness::Dead => AgentState::Dead,
                    },
                };
                AgentSnapshot {
                    name: agent.clone(),
                    state,
                    unread: messages::check(&messages_dir, scope, agent)
                        .map(|senders| senders.iter().map(|s| s.count).sum())
                        .unwrap_or(0),
                    window: pane.map(|p| p.window.clone()),
                }
            })
            .collect();
        Ok(ScopeSnapshot {
            session,
            session_exists,
            agents,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                        ..agent("main", AgentState::Busy, 2)
                    }],
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
                            "state": "busy",
                            "unread": 2,
                            "window": "app/main:1"
                        }]
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
                        "window": "app/login:implementer"
                    }]
                }]
            })
        );
        let kinds: Vec<serde_json::Value> = [
            AttentionKind::Cleanup,
            AttentionKind::Ready,
            AttentionKind::Dead,
            AttentionKind::Stalled,
            AttentionKind::None,
        ]
        .iter()
        .map(|k| serde_json::to_value(k).unwrap())
        .collect();
        assert_eq!(kinds, ["cleanup", "ready", "dead", "stalled", "none"]);
        let states: Vec<serde_json::Value> = [
            AgentState::Busy,
            AgentState::Dead,
            AgentState::Stopped,
            AgentState::Closed,
        ]
        .iter()
        .map(|s| serde_json::to_value(s).unwrap())
        .collect();
        assert_eq!(states, ["busy", "dead", "stopped", "closed"]);
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
    fn every_project_is_snapshotted_most_urgent_first_and_a_missing_one_skipped() {
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
        assert_eq!(skipped.len(), 3);
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
        assert_eq!(
            lines[2],
            format!(
                "{}: skipped (no pm project at {})",
                server.scope("gone"),
                gone.display()
            )
        );
        assert_eq!(lines.len(), 3);
    }
}
