//! Remote input seams: reading and preparing the harness's input line for
//! typed text, and the dialogs its hooks let the user answer remotely.

use std::path::Path;

use crate::harness::{Harness, claude_code, codex, opencode};
use crate::state::runtime::{Answer, Dialog, DialogRecord};

impl Harness {
    /// Whether the input line on this harness's `screen`, captured with its
    /// escape sequences, is empty and takes typed keys as text; `None` when
    /// it can't tell. `home` holds the harness's config.
    ///
    /// `config_dir` is the value the agent's environment gave
    /// [`config_dir_env`](Self::config_dir_env), if any.
    pub fn input_is_empty(
        self,
        screen: &str,
        home: &Path,
        config_dir: Option<&Path>,
    ) -> Option<bool> {
        match self {
            Harness::ClaudeCode => claude_code::input::is_empty(screen, home, config_dir),
            Harness::Codex => codex::input::is_empty(screen),
            Harness::OpenCode => opencode::input::is_empty(screen),
        }
    }

    /// The tmux key that switches an input line on `screen` into a mode that
    /// takes typed keys as text, changing nothing else, and the one that
    /// erases it should the line have taken it as text; `None` when it
    /// already does or can't. Arguments as for
    /// [`input_is_empty`](Self::input_is_empty).
    pub fn text_mode_key(
        self,
        screen: &str,
        home: &Path,
        config_dir: Option<&Path>,
    ) -> Option<(&'static str, &'static str)> {
        match self {
            Harness::ClaudeCode => claude_code::input::text_mode_key(screen, home, config_dir),
            Harness::Codex => codex::input::text_mode_key(screen),
            Harness::OpenCode => None,
        }
    }

    /// The environment variable that moves the harness's config away from
    /// its default, where the input line's settings are read, for the
    /// SessionStart hook to record from the agent's own environment.
    pub fn config_dir_env(self) -> Option<&'static str> {
        match self {
            Harness::ClaudeCode => Some(claude_code::input::CONFIG_DIR_ENV),
            Harness::Codex | Harness::OpenCode => None,
        }
    }

    /// The hook events whose payloads open a dialog the user can answer
    /// remotely ([`dialog`](Self::dialog)), for `pm harness hooks dialog`.
    /// Empty for a harness whose loop is a plugin, which calls it itself,
    /// and for codex, which runs its hooks before showing a dialog, so a
    /// hook waiting on the phone would hide the terminal's.
    pub fn dialog_events(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => &["PermissionRequest"],
            Harness::Codex | Harness::OpenCode => &[],
        }
    }

    /// The dialog a payload of `pm harness hooks dialog` opens, with what
    /// [`dialog_decision`](Self::dialog_decision) will need; `None` when
    /// none the user can answer remotely.
    pub fn dialog(self, payload: &serde_json::Value) -> Option<(Dialog, serde_json::Value)> {
        match self {
            Harness::ClaudeCode => claude_code::dialog::dialog(payload),
            Harness::Codex => None,
            Harness::OpenCode => opencode::dialog::dialog(payload),
        }
    }

    /// Whether a payload of `pm harness hooks waiting` says `record`'s
    /// dialog was answered at the terminal. Only Claude Code needs it: it
    /// leaves the hook of a dialog approved there running, while opencode's
    /// plugin ends the hook of an ask settled at the TUI.
    pub fn dialog_resolved(self, record: &DialogRecord, payload: &serde_json::Value) -> bool {
        match self {
            Harness::ClaudeCode => claude_code::dialog::resolved(record, payload),
            Harness::Codex | Harness::OpenCode => false,
        }
    }

    /// What the dialog hook prints for `answer`, which `record` accepts: the
    /// decision its harness, or plugin, applies.
    pub fn dialog_decision(self, record: &DialogRecord, answer: &Answer) -> serde_json::Value {
        match self {
            Harness::ClaudeCode => claude_code::dialog::decision(record, answer),
            // Never opens one.
            Harness::Codex => serde_json::Value::Null,
            Harness::OpenCode => opencode::dialog::decision(answer),
        }
    }
}
