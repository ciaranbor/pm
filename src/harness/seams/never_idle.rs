//! Never-idle loop seams: how the waiter wakes an agent, and what the
//! harness's hooks, prompts, and transcripts say about whether it waits.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::harness::{Harness, claude_code, codex, opencode};
use crate::state::runtime::{self, Inbound, Waiting, WaitingClass, WaitingKind};

impl Harness {
    /// How pm's Stop hook, installed for this harness, gets the
    /// continuation to the agent.
    pub fn wake(self) -> Wake {
        match self {
            Harness::ClaudeCode => claude_code::WAKE,
            Harness::Codex => codex::WAKE,
            Harness::OpenCode => opencode::WAKE,
        }
    }

    /// Whether a spawn must launch the agent with a prompt: the harness
    /// starts pm's waiter only as a turn ends, and no other way.
    pub fn needs_launch_prompt(self) -> bool {
        match self {
            Harness::Codex => codex::NEEDS_LAUNCH_PROMPT,
            Harness::ClaudeCode | Harness::OpenCode => false,
        }
    }

    /// Whether pm's SessionStart hook runs the waiter once the session has
    /// started or resumed, so the agent waits for messages without a first
    /// turn: its entry has [options](Self::session_start_hook_options) to
    /// run in the background. opencode's plugin arms at setup itself.
    pub fn waits_at_session_start(self) -> bool {
        self.session_start_hook_options().is_some()
    }

    /// Whether this harness's waiter, alive, wakes an agent at `kind`. A
    /// rewake or a post reaches a session however its turn ended; a waiter
    /// that blocks inside the turn waits only where it marked the agent
    /// idle.
    pub fn waiter_wakes(self, kind: WaitingKind) -> bool {
        match self.wake() {
            Wake::Rewake | Wake::Post => matches!(
                kind,
                WaitingKind::Idle
                    | WaitingKind::Interrupted
                    | WaitingKind::Error
                    | WaitingKind::Prompt
            ),
            Wake::Block => kind == WaitingKind::Idle,
        }
    }

    /// The channel a process outside the session posts it prompts through
    /// ([`post_prompt`](Self::post_prompt)), as its SessionStart hook,
    /// run for `agent`'s session `session_id`, finds it: in the environment
    /// the hook runs in, or the agent's runtime dir. `None` for a harness
    /// with none, which is then typed into.
    pub fn inbound(
        self,
        project_root: &Path,
        scope: &str,
        agent: &str,
        session_id: &str,
    ) -> Option<Inbound> {
        match self {
            Harness::ClaudeCode => claude_code::inbound::from_env(),
            Harness::Codex => codex::inbound(project_root, scope, agent, session_id),
            Harness::OpenCode => opencode::inbound(session_id),
        }
    }

    /// Post `text` to the session listening on `inbound` as a prompt: a
    /// turn once the session is free to take one, with no keystroke. An
    /// error when nothing listens there any more.
    pub fn post_prompt(self, inbound: &Inbound, text: &str) -> Result<()> {
        match self {
            Harness::ClaudeCode => claude_code::inbound::post(inbound, text),
            Harness::Codex => codex::post(inbound, text),
            Harness::OpenCode => opencode::post(inbound, text),
        }
    }

    /// Why this harness, as installed and set up for sessions in
    /// `worktrees`, may not wake pm's agents through their
    /// [inbound channel](Self::post_prompt), one line each.
    pub fn wake_problems(
        self,
        home: &Path,
        worktrees: &[std::path::PathBuf],
        probe: crate::harness::Probe,
    ) -> Vec<String> {
        match self {
            // Lib tests must not depend on what the machine has installed.
            Harness::ClaudeCode if cfg!(test) => {
                claude_code::inbound::problems(&[], home, worktrees, None)
            }
            Harness::ClaudeCode => claude_code::inbound::problems(
                &claude_code::inbound::MANAGED_SETTINGS
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect::<Vec<_>>(),
                home,
                worktrees,
                claude_code::installed_version(probe).as_deref(),
            ),
            Harness::Codex | Harness::OpenCode => Vec::new(),
        }
    }

    /// What a prompt UserPromptSubmit reports says: the continuation of a
    /// Stop hook's [rewake](Wake::Rewake), and a
    /// [post](Self::post_prompt), come wrapped.
    pub fn prompt_said(self, prompt: &str) -> &str {
        match self {
            Harness::ClaudeCode => claude_code::rewake_reason(prompt)
                .or_else(|| claude_code::inbound::said(prompt))
                .unwrap_or(prompt),
            Harness::Codex | Harness::OpenCode => prompt,
        }
    }

    /// Whether `prompt`, as UserPromptSubmit reports it, is one the harness
    /// wrote itself rather than the user typed: a post is another
    /// session's, wrapped by the harness.
    pub fn synthesized_prompt(self, prompt: &str) -> bool {
        match self {
            Harness::ClaudeCode => {
                claude_code::chat::is_synthesized(prompt)
                    || claude_code::inbound::said(prompt).is_some()
            }
            Harness::Codex | Harness::OpenCode => false,
        }
    }

    /// The hook events, beyond the never-idle loop's, whose payloads say
    /// when the agent waits on the user ([`waiting_event`](Self::waiting_event)).
    /// Each harness's own: an event the other lacks may make it reject the
    /// file. Empty for a harness whose loop is a plugin, which reports them
    /// itself.
    pub fn waiting_events(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => claude_code::waiting::EVENTS,
            Harness::Codex => codex::waiting::EVENTS,
            Harness::OpenCode => &[],
        }
    }

    /// What a hook payload from this harness says about the agent's waiting
    /// marker; `None` when nothing.
    pub fn waiting_event(self, payload: &serde_json::Value) -> Option<WaitingEvent> {
        match self {
            Harness::ClaudeCode => claude_code::waiting::event(payload),
            Harness::Codex => codex::waiting::event(payload),
            Harness::OpenCode => opencode::waiting::event(payload),
        }
    }

    /// The tool call a hook payload from this harness says its permission
    /// classifier refused; `None` when none, as for a harness with no
    /// classifier.
    pub fn denial(self, payload: &serde_json::Value) -> Option<runtime::Denial> {
        match self {
            Harness::ClaudeCode => claude_code::waiting::denial(payload),
            Harness::Codex | Harness::OpenCode => None,
        }
    }

    /// How the last turn of the session whose transcript is at `transcript`
    /// ended, when it ended in a way the harness fires no hook for —
    /// interrupted, or failed — and nothing has happened in it since; dated
    /// by the transcript's mtime.
    pub fn turn_ended(self, transcript: &Path) -> Option<Waiting> {
        match self {
            Harness::ClaudeCode => claude_code::transcript::turn_ended(transcript),
            Harness::Codex => codex::transcript::turn_ended(transcript),
            Harness::OpenCode => None,
        }
    }

    /// Whether the session whose transcript is at `transcript` recorded
    /// that a dialog open since `since`, one no hook reports closing, has
    /// closed; never for a harness with no such dialog.
    pub fn dialog_closed(self, transcript: &Path, since: DateTime<Utc>) -> bool {
        match self {
            Harness::ClaudeCode => claude_code::transcript::dialog_closed(transcript, since),
            Harness::Codex | Harness::OpenCode => false,
        }
    }

    /// How long after a turn ends a dialog that no hook reports can take to
    /// be drawn, for a harness that draws such dialogs between turns, or
    /// dialogs no hook reports closing, which only its screen shows
    /// ([`screen_dialog`](Self::screen_dialog)); `None` for one that draws
    /// none.
    pub fn unhooked_dialog_window(self) -> Option<std::time::Duration> {
        match self {
            Harness::ClaudeCode => Some(claude_code::screen::OPENS_WITHIN),
            Harness::Codex | Harness::OpenCode => None,
        }
    }

    /// The marker, between turns, for the dialog this harness's `screen`,
    /// captured with its escape sequences, shows beside its input line;
    /// `None` when it shows none.
    pub fn screen_dialog(self, screen: &str) -> Option<Waiting> {
        let shown = match self {
            Harness::ClaudeCode => claude_code::screen::dialog(screen)?,
            Harness::Codex | Harness::OpenCode => return None,
        };
        Some(Waiting {
            between_turns: true,
            ..Waiting::now(WaitingKind::Dialog, shown.title)
        })
    }

    /// Why this agent's never-idle loop stopped itself, if it did: pm's
    /// waiter records it, opencode's plugin in a file of its own.
    pub fn loop_stopped(self, project_root: &Path, scope: &str, agent: &str) -> Option<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => {
                runtime::loop_tripped(project_root, scope, agent)
            }
            Harness::OpenCode => {
                let file = opencode::trip_file(project_root, scope, agent).ok()?;
                let reason = std::fs::read_to_string(file).ok()?;
                Some(reason.trim().to_string())
            }
        }
    }

    /// Whether this agent's never-idle loop has loaded since its spawn.
    /// `None` for a harness whose loop is a native hook, which a running
    /// session cannot have failed to load.
    pub fn loop_loaded(self, project_root: &Path, scope: &str, agent: &str) -> Option<bool> {
        match self {
            Harness::ClaudeCode | Harness::Codex => None,
            Harness::OpenCode => Some(
                opencode::loaded_file(project_root, scope, agent).is_ok_and(|file| file.exists()),
            ),
        }
    }

    /// The error of this agent's last turn, if it failed and no turn has
    /// succeeded since. Only a harness whose loop pm emulates reports one.
    pub fn last_turn_error(self, project_root: &Path, scope: &str, agent: &str) -> Option<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => None,
            Harness::OpenCode => {
                let file = opencode::turn_error_file(project_root, scope, agent).ok()?;
                let error = std::fs::read_to_string(file).ok()?;
                Some(error.trim().to_string()).filter(|e| !e.is_empty())
            }
        }
    }
}

/// How pm's Stop hook gets the continuation to an agent
/// ([`Harness::wake`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// It waits inside the turn and answers `block`, which the harness, or
    /// the plugin that ran it, delivers.
    Block,
    /// It runs on once the turn has ended, and exits 2 with the
    /// continuation on stderr, which wakes the session.
    Rewake,
    /// It runs on once the turn has ended, and posts the continuation to
    /// the session's inbound channel ([`Harness::post_prompt`]).
    Post,
}

/// A change to an agent's waiting marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitingEvent {
    /// Replace any marker.
    Set(Waiting),
    /// Set unless the agent already has a marker of another kind whose
    /// class isn't in `over`: a vaguer report must not hide a more specific
    /// one, but a newer one replaces its own kind, which may be stale.
    Fill {
        waiting: Waiting,
        over: &'static [WaitingClass],
    },
    /// The agent is working again.
    Clear,
    /// A subagent is working again: clear a marker it set, and only that.
    ClearSubagent(String),
}
