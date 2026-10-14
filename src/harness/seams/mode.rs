//! Permission-mode seams: the modes a device may pick for an agent, and how
//! its harness's mode is read and changed. The ids are each harness's own
//! vocabulary, never shared between harnesses.

use crate::error::Result;
use crate::harness::{Harness, claude_code, codex};
use crate::state::runtime::Inbound;

/// How a harness's mode is changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeSwitch {
    /// By pressing this tmux key, which moves it one step round a cycle, the
    /// mode read back off the screen ([`Harness::mode_on_screen`]).
    Cycle(&'static str),
    /// Through the session's inbound channel ([`Harness::set_mode`]).
    Server,
}

impl Harness {
    /// The modes a device may pick, as `(id, label)`; empty for a harness
    /// pm doesn't switch.
    pub fn modes(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Harness::ClaudeCode => claude_code::mode::MODES,
            Harness::Codex => codex::mode::MODES,
            Harness::OpenCode => &[],
        }
    }

    /// How the mode is changed; `None` for a harness pm doesn't switch.
    pub fn mode_switch(self) -> Option<ModeSwitch> {
        match self {
            Harness::ClaudeCode => Some(ModeSwitch::Cycle(claude_code::mode::CYCLE_KEY)),
            Harness::Codex => Some(ModeSwitch::Server),
            Harness::OpenCode => None,
        }
    }

    /// The mode the pane showing `screen`, captured with its escape
    /// sequences, is in; `None` when the screen doesn't show it.
    pub fn mode_on_screen(self, screen: &str) -> Option<&'static str> {
        match self {
            Harness::ClaudeCode => claude_code::mode::on_screen(screen),
            Harness::Codex | Harness::OpenCode => None,
        }
    }

    /// The mode of the session on `inbound`, for a [`ModeSwitch::Server`]
    /// harness; `None` when it reports none.
    pub fn server_mode(self, inbound: &Inbound) -> Result<Option<String>> {
        match self {
            Harness::Codex => codex::mode::current(inbound),
            Harness::ClaudeCode | Harness::OpenCode => Ok(None),
        }
    }

    /// Set the session on `inbound` to `mode`, for a [`ModeSwitch::Server`]
    /// harness; the mode it then reports.
    pub fn set_mode(self, inbound: &Inbound, mode: &str) -> Result<Option<String>> {
        match self {
            Harness::Codex => codex::mode::set(inbound, mode),
            Harness::ClaudeCode | Harness::OpenCode => Ok(None),
        }
    }
}
