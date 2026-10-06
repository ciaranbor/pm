//! Which changes to a scope's attention alert the user. The tmux refresh
//! and `pm serve` share the rule; each keeps what it last judged of a scope
//! ([`Judged`]) and hands it back with the next snapshot — tmux in the
//! scope's session options, and for every feature in one global option,
//! which is all a closed feature has; serve in memory ([`Watch`]), which
//! judges closed features alongside the rest.
//!
//! A scope alerts as it enters blocked, asking or ready. A kind's episode
//! lasts while its condition holds, not while it is the scope's attention:
//! a ready feature whose agent asks, then is answered, reads ready again
//! without being newly ready, and neither does one whose agent turns busy
//! and goes idle again: a busy agent holds back only the attention, not the
//! episode, so a feature marked ready mid-turn alerts once, when its team
//! is next not busy. A scope judged for the first time — a session just
//! opened, or a watcher just started — shows a standing blocked or ready
//! without alerting it, even while a busy agent holds it back: it was set
//! before, and alerted then if anyone was watching. A dialog up now still
//! alerts.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::state::feature::{FeatureStatus, Progress};

use super::{
    AgentSnapshot, AgentState, Attention, AttentionKind, FeatureSnapshot, ScopeSnapshot, Snapshot,
    agent_attention,
};

/// The kinds that alert.
const ALERTING: [AttentionKind; 3] = [
    AttentionKind::Blocked,
    AttentionKind::Asking,
    AttentionKind::Ready,
];

/// What a scope's last judgement left behind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Judged {
    /// The attention the scope showed.
    pub attention: Option<AttentionKind>,
    /// The kinds alerted on whose condition still held.
    pub alerted: Vec<AttentionKind>,
}

impl Judged {
    /// [`Self::alerted`] as a comma-separated list, `None` when empty.
    pub fn alerted_list(&self) -> Option<String> {
        let kinds: Vec<String> = self.alerted.iter().map(ToString::to_string).collect();
        Some(kinds.join(",")).filter(|v| !v.is_empty())
    }

    /// The alerting kinds named in an [`Self::alerted_list`].
    pub fn parse_alerted(list: &str) -> Vec<AttentionKind> {
        ALERTING
            .into_iter()
            .filter(|k| list.split(',').any(|r| r == k.to_string()))
            .collect()
    }

    /// The alerting kind `shown` names; any other kind never decides an
    /// alert, so it reads as none.
    pub fn parse_attention(shown: &str) -> Option<AttentionKind> {
        ALERTING.into_iter().find(|k| k.to_string() == shown)
    }

    fn record(&mut self, kind: AttentionKind) {
        if !self.alerted.contains(&kind) {
            self.alerted.push(kind);
        }
    }
}

/// A scope's judgement: what to keep for the next, and whether to alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub judged: Judged,
    pub alert: bool,
}

/// Judge `feature` against what its last judgement left, `None` when it has
/// had none.
pub fn judge_feature(previous: Option<&Judged>, feature: &FeatureSnapshot) -> Verdict {
    let kind = feature.attention.kind;
    let mut judged = carried(previous, |k| feature_holds(feature, k));
    if previous.is_none() {
        for standing in [AttentionKind::Blocked, AttentionKind::Ready] {
            if feature_holds(feature, standing) {
                judged.record(standing);
            }
        }
    }
    let shown = previous.and_then(|p| p.attention);
    let alert = ALERTING.contains(&kind) && shown != Some(kind) && !judged.alerted.contains(&kind);
    if alert {
        judged.record(kind);
    }
    judged.attention = (kind != AttentionKind::None).then_some(kind);
    Verdict { judged, alert }
}

/// Judge a main scope, which alerts only on an agent asking.
pub fn judge_main(previous: Option<&Judged>, main: &ScopeSnapshot) -> Verdict {
    let kind = main.attention.kind;
    let mut judged = carried(previous, |k| main_holds(&main.agents, k));
    let shown = previous.and_then(|p| p.attention);
    let alert =
        kind == AttentionKind::Asking && shown != Some(kind) && !judged.alerted.contains(&kind);
    if alert {
        judged.record(kind);
    }
    judged.attention = (kind != AttentionKind::None).then_some(kind);
    Verdict { judged, alert }
}

/// The kinds `previous` alerted on whose condition still `holds`.
fn carried(previous: Option<&Judged>, holds: impl Fn(AttentionKind) -> bool) -> Judged {
    Judged {
        alerted: previous
            .map(|p| p.alerted.iter().copied().filter(|k| holds(*k)).collect())
            .unwrap_or_default(),
        ..Judged::default()
    }
}

/// Whether what makes `feature` need `kind` still holds, whatever outranks it.
fn feature_holds(feature: &FeatureSnapshot, kind: AttentionKind) -> bool {
    match kind {
        AttentionKind::Blocked => feature.progress == Progress::Blocked,
        AttentionKind::Ready => {
            feature.progress == Progress::Ready || feature.lifecycle == FeatureStatus::Approved
        }
        AttentionKind::Asking => asking(&feature.agents),
        _ => false,
    }
}

fn main_holds(agents: &[AgentSnapshot], kind: AttentionKind) -> bool {
    kind == AttentionKind::Asking && asking(agents)
}

fn asking(agents: &[AgentSnapshot]) -> bool {
    agents.iter().any(|a| a.state == AgentState::Asking)
}

/// A scope entering a state that alerts, or one of its agents dying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Transition {
    pub project: String,
    /// The feature, or `main`.
    pub scope: String,
    #[serde(flatten)]
    pub attention: Attention,
}

type ScopeKey = (String, String);

/// What a watcher of every scope last judged. A scope it has not read
/// since it started, or since its project was last unreadable, is judged
/// as never before; one that appears later is new, so a standing state it
/// shows does alert. An agent dies when it reads dead and did not before.
#[derive(Debug, Default)]
pub struct Watch {
    started: bool,
    scopes: HashMap<ScopeKey, Judged>,
    dead: HashSet<(String, String, String)>,
    /// The projects the last snapshot couldn't read.
    unread: HashSet<String>,
}

impl Watch {
    /// A watch that has seen `snapshot`, with nothing to report about it.
    pub fn start(snapshot: &Snapshot) -> Self {
        Self::default().observe(snapshot).0
    }

    /// The watch after `snapshot`, and the transitions into it.
    pub fn observe(&self, snapshot: &Snapshot) -> (Self, Vec<Transition>) {
        let mut next = Self {
            started: true,
            ..Self::default()
        };
        let mut transitions = Vec::new();
        let new = Judged::default();
        let unseen = |project: &str| !self.started || self.unread.contains(project);
        let previous = |key: &ScopeKey| match self.scopes.get(key) {
            Some(judged) => Some(judged),
            None if unseen(&key.0) => None,
            None => Some(&new),
        };
        let mut scopes: Vec<(ScopeKey, Verdict, &Attention, &[AgentSnapshot])> = Vec::new();
        for f in &snapshot.features {
            let key = (f.project.clone(), f.name.clone());
            let verdict = judge_feature(previous(&key), f);
            scopes.push((key, verdict, &f.attention, &f.agents));
        }
        for p in &snapshot.projects {
            let Some(main) = &p.main else {
                // Unread this time: keep what was known, so it isn't new
                // when it reads again.
                next.scopes.extend(
                    self.scopes
                        .iter()
                        .filter(|((project, _), _)| *project == p.name)
                        .map(|(k, v)| (k.clone(), v.clone())),
                );
                next.dead
                    .extend(self.dead.iter().filter(|(pr, _, _)| *pr == p.name).cloned());
                next.unread.insert(p.name.clone());
                continue;
            };
            let key = (p.name.clone(), "main".to_string());
            let verdict = judge_main(previous(&key), main);
            scopes.push((key, verdict, &main.attention, &main.agents));
        }
        for ((project, scope), verdict, attention, agents) in scopes {
            let known = previous(&(project.clone(), scope.clone())).is_some();
            if verdict.alert {
                transitions.push(Transition {
                    project: project.clone(),
                    scope: scope.clone(),
                    attention: attention.clone(),
                });
            }
            for agent in agents.iter().filter(|a| a.state == AgentState::Dead) {
                let key = (project.clone(), scope.clone(), agent.name.clone());
                if known && !self.dead.contains(&key) {
                    transitions.push(Transition {
                        project: project.clone(),
                        scope: scope.clone(),
                        attention: agent_attention(agent, AttentionKind::Dead),
                    });
                }
                next.dead.insert(key);
            }
            next.scopes.insert((project, scope), verdict.judged);
        }
        (next, transitions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::attention::{ProjectSnapshot, VERSION, WaitingSnapshot, attention};
    use crate::state::runtime::WaitingKind;

    fn agent(state: AgentState) -> AgentSnapshot {
        AgentSnapshot {
            name: "implementer".into(),
            state,
            unread: 0,
            window: Some("app/login:1".into()),
            pane: None,
            waiting: (state == AgentState::Asking).then(|| WaitingSnapshot {
                kind: WaitingKind::Permission,
                detail: "permission".into(),
                since: None,
                dialog: None,
            }),
        }
    }

    fn feature(progress: Progress, busy: bool, state: AgentState) -> FeatureSnapshot {
        let mut f = FeatureSnapshot {
            project: "app".into(),
            name: "login".into(),
            attention: Attention {
                kind: AttentionKind::None,
                detail: None,
                agent: None,
            },
            progress,
            blocked_reason: None,
            blocked_by: None,
            summary: Some("Adds login".into()),
            lifecycle: FeatureStatus::Review,
            pr: None,
            session: "app/login".into(),
            session_exists: true,
            agents: vec![agent(state)],
            working: busy,
            background_since: None,
            last_activity: None,
        };
        f.attention = attention(&f);
        f
    }

    fn snapshot(features: Vec<FeatureSnapshot>, main: Option<AgentState>) -> Snapshot {
        let main = main.map(|state| {
            let agents = vec![AgentSnapshot {
                name: "main".into(),
                ..agent(state)
            }];
            ScopeSnapshot {
                session: "app/main".into(),
                session_exists: true,
                attention: super::super::main_attention(&agents),
                agents,
                working: false,
                background_since: None,
                last_activity: None,
            }
        });
        Snapshot {
            version: VERSION,
            projects: vec![ProjectSnapshot {
                name: "app".into(),
                root: "/src/app".into(),
                skipped: None,
                main,
            }],
            features,
        }
    }

    #[test]
    fn a_watch_reports_no_transitions_for_what_it_started_on() {
        let blocked = snapshot(
            vec![feature(Progress::Blocked, false, AgentState::Dead)],
            Some(AgentState::Asking),
        );
        let watch = Watch::start(&blocked);
        assert!(watch.observe(&blocked).1.is_empty());

        let ready = snapshot(
            vec![feature(Progress::Ready, false, AgentState::Dead)],
            Some(AgentState::Idle),
        );
        let (watch, transitions) = watch.observe(&ready);
        let kinds: Vec<(&str, AttentionKind)> = transitions
            .iter()
            .map(|t| (t.scope.as_str(), t.attention.kind))
            .collect();
        assert_eq!(kinds, [("login", AttentionKind::Ready)]);
        assert!(watch.observe(&ready).1.is_empty());
    }

    #[test]
    fn a_watch_reports_an_agent_dying_and_a_new_scopes_standing_state() {
        let watch = Watch::start(&snapshot(Vec::new(), Some(AgentState::Idle)));
        let (watch, transitions) = watch.observe(&snapshot(
            vec![feature(Progress::Blocked, false, AgentState::Idle)],
            Some(AgentState::Dead),
        ));
        assert_eq!(
            transitions,
            [
                Transition {
                    project: "app".into(),
                    scope: "login".into(),
                    attention: Attention {
                        kind: AttentionKind::Blocked,
                        detail: None,
                        agent: None,
                    },
                },
                Transition {
                    project: "app".into(),
                    scope: "main".into(),
                    attention: Attention {
                        kind: AttentionKind::Dead,
                        detail: Some("main: harness exited".into()),
                        agent: Some("main".into()),
                    },
                },
            ]
        );
        let (_, again) = watch.observe(&snapshot(
            vec![feature(Progress::Blocked, false, AgentState::Idle)],
            Some(AgentState::Dead),
        ));
        assert!(again.is_empty());
    }

    #[test]
    fn a_project_unreadable_until_now_reports_nothing_that_was_already_so() {
        let unreadable = |mut s: Snapshot| {
            s.features.clear();
            s.projects[0].main = None;
            s.projects[0].skipped = Some("broken".into());
            s
        };
        let standing = || {
            snapshot(
                vec![feature(Progress::Blocked, false, AgentState::Idle)],
                Some(AgentState::Dead),
            )
        };

        let watch = Watch::start(&unreadable(standing()));
        let (_, transitions) = watch.observe(&standing());
        assert!(transitions.is_empty(), "{transitions:?}");

        let alive = snapshot(
            vec![feature(Progress::Blocked, false, AgentState::Idle)],
            Some(AgentState::Idle),
        );
        let (watch, _) = Watch::start(&alive).observe(&unreadable(standing()));
        let ready = snapshot(
            vec![feature(Progress::Ready, false, AgentState::Idle)],
            Some(AgentState::Dead),
        );
        let (_, transitions) = watch.observe(&ready);
        let kinds: Vec<(&str, AttentionKind)> = transitions
            .iter()
            .map(|t| (t.scope.as_str(), t.attention.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("login", AttentionKind::Ready),
                ("main", AttentionKind::Dead)
            ],
            "known before it went unreadable"
        );
    }

    #[test]
    fn a_standing_ready_first_judged_while_an_agent_is_busy_never_alerts() {
        let busy = judge_feature(None, &feature(Progress::Ready, true, AgentState::Busy));
        assert_eq!(busy.judged.attention, None);
        let idle = feature(Progress::Ready, false, AgentState::Idle);
        let verdict = judge_feature(Some(&busy.judged), &idle);
        assert_eq!(verdict.judged.attention, Some(AttentionKind::Ready));
        assert!(!verdict.alert);
    }

    #[test]
    fn a_transition_serializes_flat() {
        let t = Transition {
            project: "app".into(),
            scope: "login".into(),
            attention: Attention {
                kind: AttentionKind::Blocked,
                detail: Some("which DB?".into()),
                agent: Some("implementer".into()),
            },
        };
        assert_eq!(
            serde_json::to_value(&t).unwrap(),
            serde_json::json!({
                "project": "app",
                "scope": "login",
                "kind": "blocked",
                "detail": "which DB?",
                "agent": "implementer"
            })
        );
    }
}
