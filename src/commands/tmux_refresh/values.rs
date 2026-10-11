//! The option values a refresh publishes for a session, window, or the
//! server.

use chrono::{DateTime, Utc};

use crate::tmux::options::format_text;

use super::super::attention::{
    self, Activity, AgentSnapshot, AgentState, Attention, AttentionKind, FeatureSnapshot,
    ScopeSnapshot, transition::Judged,
};
use super::super::feat_status_view::{STALLED, span};
use super::{
    ACTIVITY, ACTIVITY_LABEL, AGENT, AGENT_BADGE, AGENT_LABEL, AGENT_STATE, ALERTED, ATTENTION,
    BADGE, COUNT, ENTRY_SEPARATOR, FEATURE, FEATURES_ALERTED, LABEL, PROGRESS, PROJECT, REASON,
    SUMMARY, UNREAD, badge,
};

pub(super) fn session_values(
    feature: &FeatureSnapshot,
    judged: &Judged,
    now: DateTime<Utc>,
) -> Vec<(&'static str, Option<String>)> {
    let kind = feature.attention.kind;
    let needs = (kind != AttentionKind::None).then_some(kind);
    let [activity, activity_label] = attention::activity(
        feature.working,
        feature.background_since,
        feature.last_activity,
        now,
    )
    .map(|a| drawn(a, now).map(Some))
    .unwrap_or_default();
    vec![
        (PROJECT, Some(format_text(&feature.project))),
        (FEATURE, Some(format_text(&feature.name))),
        (PROGRESS, Some(feature.progress.to_string())),
        (REASON, reason(&feature.attention)),
        (ATTENTION, judged.attention.map(|k| k.to_string())),
        (BADGE, needs.and_then(badge::attention)),
        (LABEL, needs.and_then(badge::attention_label)),
        (ACTIVITY, activity),
        (ACTIVITY_LABEL, activity_label),
        (ALERTED, judged.alerted_list()),
    ]
}

/// The attention detail; a stalled scope has none of its own, so it gets
/// what the status view says.
fn reason(attention: &Attention) -> Option<String> {
    let detail = match attention.kind {
        AttentionKind::Stalled => Some(STALLED),
        _ => attention.detail.as_deref(),
    };
    detail.map(format_text).filter(|r| !r.is_empty())
}

/// A main session has no feature. Its badge is its `orchestrator`'s (the
/// agent [`crate::state::scope::orchestrator`] names), unless it needs
/// something: an asking main shows the asking agent's badge, which shows the
/// question, and a blocked one, as a feature's, its attention's.
pub(super) fn main_values(
    project: &str,
    main: &ScopeSnapshot,
    orchestrator: &str,
    judged: &Judged,
    now: DateTime<Utc>,
) -> Vec<(&'static str, Option<String>)> {
    let named = |name: &str| main.agents.iter().find(|a| a.name == name);
    let lead = named(orchestrator).or(main.agents.first());
    let shown = match (main.attention.kind, main.attention.agent.as_deref()) {
        (AttentionKind::Blocked, _) => None,
        (AttentionKind::Asking, Some(asking)) => named(asking).or(lead),
        _ => lead,
    };
    let [activity, activity_label] = main_activity(main, shown, now)
        .map(|a| drawn(a, now).map(Some))
        .unwrap_or_default();
    let (badge, label) = match shown {
        Some(a) => (
            Some(badge::agent(a.state, a.unread)),
            Some(badge::agent_label(a.state, a.unread)),
        ),
        None if main.attention.kind == AttentionKind::Blocked => (
            badge::attention(AttentionKind::Blocked),
            badge::attention_label(AttentionKind::Blocked),
        ),
        None => (None, None),
    };
    vec![
        (PROJECT, Some(format_text(project))),
        (FEATURE, None),
        (PROGRESS, Some(main.progress.to_string())),
        (REASON, reason(&main.attention)),
        (ATTENTION, judged.attention.map(|k| k.to_string())),
        (BADGE, badge),
        (LABEL, label),
        (ACTIVITY, activity),
        (ACTIVITY_LABEL, activity_label),
        (ALERTED, judged.alerted_list()),
    ]
}

/// A main scope's activity, without the busy glyph its badge already shows
/// when that is the `shown` agent's. Background work still shows, for its
/// age.
fn main_activity(
    main: &ScopeSnapshot,
    shown: Option<&AgentSnapshot>,
    now: DateTime<Utc>,
) -> Option<Activity> {
    let shown_busy = shown.is_some_and(|a| a.state == AgentState::Busy);
    attention::activity(main.working, main.background_since, main.last_activity, now)
        .filter(|a| !(*a == Activity::Working && shown_busy))
}

/// `activity` as `@pm_activity` and `@pm_activity_label`: the busy glyph,
/// the background glyph and wait (`1d`), or the quiet spell (`2h`); then
/// the same with words.
fn drawn(activity: Activity, now: DateTime<Utc>) -> [String; 2] {
    match activity {
        Activity::Working => [badge::working(), badge::working_label()],
        Activity::Background(since) => {
            let span = span(since, now);
            [
                badge::background(&span),
                badge::background(&format!("background {span}")),
            ]
        }
        Activity::Quiet(since) => {
            let span = span(since, now);
            [
                badge::styled(QUIET_STYLE, &span),
                badge::styled(QUIET_STYLE, &format!("quiet {span}")),
            ]
        }
    }
}

const QUIET_STYLE: &str = "fg=colour245";

pub(crate) fn window_values(agent: &AgentSnapshot) -> Vec<(&'static str, Option<String>)> {
    vec![
        (AGENT, Some(format_text(&agent.name))),
        (AGENT_STATE, Some(agent.state.to_string())),
        (UNREAD, Some(agent.unread.to_string())),
        (AGENT_BADGE, Some(badge::agent(agent.state, agent.unread))),
        (
            AGENT_LABEL,
            Some(badge::agent_label(agent.state, agent.unread)),
        ),
    ]
}

/// The totals of `kinds`, the attention of each session that publishes
/// one, so the count matches what the attention tree lists.
/// The `(session, alerted kinds)` entries of a [`FEATURES_ALERTED`] value,
/// which keeps every feature's alerted kinds so a closed feature, which has
/// no session to hold them, is judged against them too.
pub(super) fn features_alerted(value: &str) -> Vec<(&str, &str)> {
    value
        .split(ENTRY_SEPARATOR)
        .filter_map(|entry| entry.rsplit_once('='))
        .collect()
}

pub(super) fn feature_entry(session: &str, kinds: &str) -> String {
    format!("{session}={kinds}")
}

pub(super) fn global_values(
    mut kinds: Vec<AttentionKind>,
    record: &[String],
) -> Vec<(&'static str, Option<String>)> {
    kinds.sort();
    let summary: Vec<String> = kinds
        .chunk_by(|a, b| a == b)
        .filter_map(|run| badge::attention_count(run[0], run.len()))
        .collect();
    vec![
        (SUMMARY, Some(summary.join(" · ")).filter(|s| !s.is_empty())),
        (COUNT, Some(kinds.len().to_string())),
        (
            FEATURES_ALERTED,
            Some(record.join(ENTRY_SEPARATOR)).filter(|v| !v.is_empty()),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stalled_scope_carries_a_reason_though_its_attention_has_none() {
        let stalled = Attention {
            kind: AttentionKind::Stalled,
            detail: None,
            agent: None,
        };
        assert_eq!(reason(&stalled).as_deref(), Some(STALLED));
    }

    #[test]
    fn mains_badge_is_its_orchestrators_whatever_it_is_called() {
        let agent = |name: &str, state| AgentSnapshot {
            name: name.into(),
            state,
            unread: 0,
            window: None,
            pane: None,
            waiting: None,
        };
        let main = ScopeSnapshot::main_of(
            vec![
                agent("main", AgentState::Busy),
                agent("boss", AgentState::Idle),
            ],
            Default::default(),
        );
        let badge_of = |orchestrator| {
            main_values("app", &main, orchestrator, &Judged::default(), Utc::now())
                .into_iter()
                .find(|(option, _)| *option == BADGE)
                .and_then(|(_, value)| value)
        };
        assert_eq!(badge_of("boss"), Some(badge::agent(AgentState::Idle, 0)));
        assert_eq!(badge_of("main"), Some(badge::agent(AgentState::Busy, 0)));
    }

    #[test]
    fn activity_is_drawn_with_its_glyph_and_age() {
        let now = Utc::now();
        let ago = |minutes| now - chrono::Duration::minutes(minutes);
        assert_eq!(
            drawn(Activity::Working, now),
            [
                "#[fg=green]\u{f013}#[default]",
                "#[fg=green]\u{f013} working#[default]"
            ]
        );
        assert_eq!(
            drawn(Activity::Background(ago(1500)), now),
            [
                "#[fg=green]\u{f110} 1d#[default]",
                "#[fg=green]\u{f110} background 1d#[default]"
            ]
        );
        assert_eq!(
            drawn(Activity::Quiet(ago(185)), now),
            [
                "#[fg=colour245]3h#[default]",
                "#[fg=colour245]quiet 3h#[default]"
            ]
        );
    }
}
