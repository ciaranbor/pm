//! Which kinds a scope has already alerted on. A kind's episode lasts while
//! its condition holds, not while it is the scope's attention: a ready
//! feature whose agent asks, then is answered, reads `ready` again without
//! being newly ready. The record is published as a session option, as the
//! rest of a refresh's previous state is.

use crate::tmux::options::Holder;

use super::super::attention::{AgentSnapshot, AgentState, AttentionKind, FeatureSnapshot};
use crate::state::feature::{FeatureStatus, Progress};

pub(super) const ALERTED: &str = "@pm_alerted";

/// The kinds that alert.
const ALERTING: [AttentionKind; 3] = [
    AttentionKind::Blocked,
    AttentionKind::Asking,
    AttentionKind::Ready,
];

pub(super) struct Episode(Vec<AttentionKind>);

impl Episode {
    /// The kinds `held` records as alerted on whose condition still `holds`.
    pub(super) fn read(held: &Holder, holds: impl Fn(AttentionKind) -> bool) -> Self {
        let recorded = held.get(ALERTED);
        Self(
            ALERTING
                .into_iter()
                .filter(|k| recorded.split(',').any(|r| r == k.to_string()))
                .filter(|k| holds(*k))
                .collect(),
        )
    }

    pub(super) fn alerted(&self, kind: AttentionKind) -> bool {
        self.0.contains(&kind)
    }

    pub(super) fn record(&mut self, kind: AttentionKind) {
        if !self.alerted(kind) {
            self.0.push(kind);
        }
    }

    pub(super) fn value(&self) -> Option<String> {
        let kinds: Vec<String> = self.0.iter().map(ToString::to_string).collect();
        Some(kinds.join(",")).filter(|v| !v.is_empty())
    }
}

/// Whether what makes `feature` need `kind` still holds, whatever outranks it.
pub(super) fn feature_holds(feature: &FeatureSnapshot, kind: AttentionKind) -> bool {
    match kind {
        AttentionKind::Blocked => feature.progress == Progress::Blocked,
        AttentionKind::Ready => {
            feature.progress == Progress::Ready || feature.lifecycle == FeatureStatus::Approved
        }
        AttentionKind::Asking => asking(&feature.agents),
        _ => false,
    }
}

/// Whether what makes a main scope of `agents` need `kind` still holds.
pub(super) fn main_holds(agents: &[AgentSnapshot], kind: AttentionKind) -> bool {
    kind == AttentionKind::Asking && asking(agents)
}

fn asking(agents: &[AgentSnapshot]) -> bool {
    agents.iter().any(|a| a.state == AgentState::Asking)
}
