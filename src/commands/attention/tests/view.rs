use chrono::Utc;

use super::*;
use crate::state::feature::{FeatureStatus, Progress};
use crate::state::runtime::{Latest, WaitingKind};
use crate::state::scope::TeamStatus;

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
    f.agents = vec![AgentSnapshot {
        latest: Some(Latest {
            text: "Which database should login use?".into(),
            at: Some("2026-10-02T09:29:00Z".parse().unwrap()),
        }),
        ..agent("implementer", AgentState::Idle, 0)
    }];
    f.attention = attention(&f);
    let snapshot = Snapshot {
        version: VERSION,
        projects: vec![ProjectSnapshot {
            name: "app".into(),
            root: "/src/app".into(),
            skipped: None,
            home: false,
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
                progress: Progress::Wip,
                blocked_reason: None,
                blocked_by: None,
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
                "home": false,
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
                        },
                        "latest": null
                    }],
                    "attention": {
                        "kind": "asking",
                        "detail": "main: plan approval",
                        "agent": "main"
                    },
                    "progress": "wip",
                    "blocked_reason": null,
                    "blocked_by": null,
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
                    "waiting": null,
                    "latest": {
                        "text": "Which database should login use?",
                        "at": "2026-10-02T09:29:00Z"
                    }
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
        AttentionKind::Denied,
        AttentionKind::Stalled,
        AttentionKind::None,
    ]
    .iter()
    .map(|k| serde_json::to_value(k).unwrap())
    .collect();
    assert_eq!(
        kinds,
        [
            "cleanup", "ready", "dead", "unarmed", "denied", "stalled", "none"
        ]
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
fn an_asking_agent_ranks_before_everything_else() {
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
    assert_eq!(kind(&f), AttentionKind::Asking);
    f.agents.pop();
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
fn a_denial_needs_the_user_once_its_agent_is_between_turns() {
    let denied = |name: &str, state| AgentSnapshot {
        denied: Some("Bash: rm -rf build [Irreversible Local Destruction]".into()),
        ..agent(name, state, 0)
    };
    let mut f = feature(Progress::Wip, FeatureStatus::Wip);
    f.agents = vec![
        agent("reviewer", AgentState::Idle, 0),
        denied("implementer", AgentState::Busy),
    ];
    assert_eq!(
        kind(&f),
        AttentionKind::None,
        "a busy agent works around it"
    );
    f.agents.push(denied("qa", AgentState::Background));
    assert_eq!(
        attention(&f),
        Attention {
            kind: AttentionKind::Denied,
            detail: Some("qa: Bash: rm -rf build [Irreversible Local Destruction]".into()),
            agent: Some("qa".into()),
        }
    );
    f.agents.push(waiting(
        "lead",
        AgentState::Unarmed,
        WaitingKind::Error,
        "API error",
    ));
    assert_eq!(kind(&f), AttentionKind::Unarmed);

    let main = |agents| ScopeSnapshot::main_of(agents, TeamStatus::default());
    assert_eq!(
        main(vec![denied("main", AgentState::Busy)]).attention.kind,
        AttentionKind::None
    );
    assert_eq!(
        main(vec![denied("main", AgentState::Idle)]).attention.kind,
        AttentionKind::Denied
    );
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
    let main = |agents: Vec<AgentSnapshot>| ScopeSnapshot::main_of(agents, TeamStatus::default());
    let kind_of = |agents: Vec<AgentSnapshot>| main(agents).attention.kind;

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
        main(vec![unarmed, dead, asking]).attention,
        Attention {
            kind: AttentionKind::Asking,
            detail: Some("c: plan approval".into()),
            agent: Some("c".into()),
        }
    );
}

#[test]
fn a_blocked_main_needs_its_answer_after_any_dialog_and_an_idle_one_never_stalls() {
    let asking = waiting("c", AgentState::Asking, WaitingKind::Plan, "plan approval");
    let dead = agent("c", AgentState::Dead, 0);
    let blocked = TeamStatus {
        progress: Progress::Blocked,
        blocked_reason: Some("next item?".into()),
        blocked_by: Some("main".into()),
    };

    assert_eq!(
        ScopeSnapshot::main_of(vec![asking], blocked.clone())
            .attention
            .kind,
        AttentionKind::Asking
    );
    assert_eq!(
        ScopeSnapshot::main_of(vec![dead], blocked).attention,
        Attention {
            kind: AttentionKind::Blocked,
            detail: Some("next item?".into()),
            agent: Some("main".into()),
        }
    );
    let idle = vec![agent("main", AgentState::Idle, 0)];
    assert_eq!(
        ScopeSnapshot::main_of(idle, TeamStatus::default())
            .attention
            .kind,
        AttentionKind::None
    );
}
