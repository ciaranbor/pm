mod read;
mod view;

use super::*;
use crate::state::feature::{FeatureStatus, Progress};
use crate::state::runtime::WaitingKind;

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
        latest: None,
        denied: None,
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
