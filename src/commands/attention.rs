//! The attention snapshot behind `pm feat status` and `pm status`: every
//! feature, what its agents are doing, and what — if anything — it or a
//! project's main scope needs from the user (README, "Follow what needs
//! you", has the ranking; docs/remote-api.md, "Attention snapshot", the
//! JSON contract).

mod latest;
mod read;
pub mod transition;
mod view;

pub use latest::WIDTH as LATEST_WIDTH;
pub use read::{all, project, scope_agents, scope_agents_in, scope_latest};
pub use view::{
    Activity, AgentSnapshot, AgentState, Attention, AttentionKind, FeatureSnapshot,
    ProjectSnapshot, QUIET_SECS, ScopeSnapshot, Snapshot, VERSION, WORKING_SECS, WaitingSnapshot,
    activity, attention, main_attention,
};

#[cfg(test)]
mod tests;
