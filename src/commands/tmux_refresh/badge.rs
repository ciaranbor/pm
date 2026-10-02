//! The styled glyphs pm publishes: an agent's state, and the attention a
//! scope needs. An attention kind that means what an agent state means
//! (asking, dead, unarmed) is drawn as that state, so one glyph reads the
//! same on a window and on its session.
//!
//! Glyphs are Nerd Font (v3), one cell wide. A glyph inside a badge is
//! followed by a space: some terminals (Ghostty) draw an icon at full size
//! only when the next cell is blank. Styles colour only the foreground, so
//! a glyph sits on the surrounding background, and use named and
//! 256-palette colours only, which every tmux release draws.

use super::super::attention::{AgentState, AttentionKind};

/// The badge of an agent in `state` with `unread` messages. It resets to
/// the base style once, at the end.
pub(super) fn agent(state: AgentState, unread: u32) -> String {
    let (style, glyph) = agent_mark(state);
    let mut badge = format!("#[{style}]{glyph}");
    if unread > 0 {
        // nf-fa-envelope
        badge.push_str(" #[fg=yellow]\u{f0e0}");
    }
    badge + "#[default]"
}

/// The badge of a scope needing `kind`; `None` needs nothing.
pub(super) fn attention(kind: AttentionKind) -> Option<String> {
    let (style, glyph) = attention_mark(kind)?;
    Some(styled(style, glyph))
}

/// `count` scopes needing `kind`, as the summary lists them.
pub(super) fn attention_count(kind: AttentionKind, count: usize) -> Option<String> {
    let (style, glyph) = attention_mark(kind)?;
    Some(styled(style, &format!("{glyph} {count}")))
}

/// The busy glyph, for a scope that is working.
pub(super) fn working() -> String {
    let (style, glyph) = agent_mark(AgentState::Busy);
    styled(style, glyph)
}

/// `text` in `style`, then back to the surrounding style.
pub(super) fn styled(style: &str, text: &str) -> String {
    format!("#[{style}]{text}#[default]")
}

/// nf-fa-gear, nf-fa-question_circle, nf-fa-bell_slash, nf-fa-spinner,
/// nf-fa-hourglass_half, nf-md-skull, nf-fa-stop.
fn agent_mark(state: AgentState) -> (&'static str, &'static str) {
    match state {
        AgentState::Busy => ("fg=green", "\u{f013}"),
        AgentState::Asking => ("fg=red,bold", "\u{f059}"),
        AgentState::Unarmed => ("fg=magenta", "\u{f1f6}"),
        AgentState::Background => ("fg=green", "\u{f110}"),
        AgentState::Idle => ("fg=colour245", "\u{f252}"),
        AgentState::Dead => ("fg=red", "\u{f068c}"),
        AgentState::Stopped | AgentState::Closed => ("fg=colour245", "\u{f04d}"),
    }
}

/// nf-fa-hand, nf-md-broom, nf-fa-check_circle, nf-fa-pause.
fn attention_mark(kind: AttentionKind) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        AttentionKind::Blocked => ("fg=red,bold", "\u{f256}"),
        AttentionKind::Asking => agent_mark(AgentState::Asking),
        AttentionKind::Cleanup => ("fg=colour245", "\u{f00e2}"),
        AttentionKind::Ready => ("fg=green,bold", "\u{f058}"),
        AttentionKind::Dead => agent_mark(AgentState::Dead),
        AttentionKind::Unarmed => agent_mark(AgentState::Unarmed),
        AttentionKind::Stalled => ("fg=yellow", "\u{f04c}"),
        AttentionKind::None => return None,
    })
}
