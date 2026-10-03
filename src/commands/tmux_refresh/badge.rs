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

/// nf-fa-envelope
const ENVELOPE: &str = "\u{f0e0}";

/// The badge of an agent in `state` with `unread` messages. It resets to
/// the base style once, at the end.
pub(super) fn agent(state: AgentState, unread: u32) -> String {
    let (style, glyph) = agent_mark(state);
    let mut badge = format!("#[{style}]{glyph}");
    if unread > 0 {
        badge.push_str(&format!(" #[fg=yellow]{ENVELOPE}"));
    }
    badge + "#[default]"
}

/// [`agent`] with words, for where there is room: the state after its
/// glyph, and the unread count after the envelope.
pub(super) fn agent_label(state: AgentState, unread: u32) -> String {
    let (style, glyph) = agent_mark(state);
    let mut label = styled(style, &format!("{glyph} {state}"));
    if unread > 0 {
        label.push(' ');
        label.push_str(&styled("fg=yellow", &format!("{ENVELOPE} {unread}")));
    }
    label
}

/// The badge of a scope needing `kind`; `None` needs nothing.
pub(super) fn attention(kind: AttentionKind) -> Option<String> {
    let (style, glyph) = attention_mark(kind)?;
    Some(styled(style, glyph))
}

/// [`attention`] with the kind after its glyph.
pub(super) fn attention_label(kind: AttentionKind) -> Option<String> {
    let (style, glyph) = attention_mark(kind)?;
    Some(styled(style, &format!("{glyph} {kind}")))
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

/// [`working`] with the word after its glyph.
pub(super) fn working_label() -> String {
    let (style, glyph) = agent_mark(AgentState::Busy);
    styled(style, &format!("{glyph} working"))
}

/// The background glyph and `text`, for a scope waiting on background
/// work.
pub(super) fn background(text: &str) -> String {
    let (style, glyph) = agent_mark(AgentState::Background);
    styled(style, &format!("{glyph} {text}"))
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
