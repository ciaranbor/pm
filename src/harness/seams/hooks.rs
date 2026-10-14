//! Hook seams: where pm's hooks or plugin install, the trust the harness
//! needs before it runs them, and what the SessionStart hook prints.

use std::path::{Path, PathBuf};

use crate::harness::{Harness, HookTrust, Probe, claude_code, codex, opencode};
use crate::state::project::HarnessConfig;

impl Harness {
    /// The harness's user-level file that pm installs its hooks into, once
    /// per machine (`~/.claude/settings.json`, `$CODEX_HOME/hooks.json`).
    /// Both take the same nested `hooks` shape. `None` for a harness
    /// whose loop is not a hooks entry; see
    /// [`plugin_files`](Self::plugin_files).
    pub fn user_settings_file(self, home: &Path) -> Option<PathBuf> {
        let dir = self.global_config_dir(home)?;
        match self {
            Harness::ClaudeCode => Some(dir.join(claude_code::USER_SETTINGS_FILE)),
            Harness::Codex => Some(dir.join(codex::HOOKS_FILE)),
            Harness::OpenCode => None,
        }
    }

    /// Files pm owns outright in the harness's user-level dir and writes
    /// verbatim, once per machine, with their bundled content — the
    /// counterpart of [`user_settings_file`](Self::user_settings_file) for a
    /// harness whose never-idle loop is a plugin. Empty for the others.
    pub fn plugin_files(self, home: &Path) -> Vec<(PathBuf, &'static str)> {
        match self {
            Harness::ClaudeCode | Harness::Codex => Vec::new(),
            Harness::OpenCode => opencode::plugin_files(home),
        }
    }

    /// Whether a running session picks up an edit to pm's never-idle loop
    /// — its hook entries or plugin files — without a restart.
    pub fn hooks_reload_live(self) -> bool {
        match self {
            Harness::ClaudeCode => true,
            Harness::Codex | Harness::OpenCode => false,
        }
    }

    /// The keys pm's Stop hook entry carries beyond its command and
    /// timeout, which make the harness run it once the turn has ended.
    pub fn stop_hook_options(self) -> serde_json::Map<String, serde_json::Value> {
        hook_options(match self {
            Harness::ClaudeCode => claude_code::STOP_HOOK_OPTIONS,
            Harness::Codex => codex::STOP_HOOK_OPTIONS,
            Harness::OpenCode => &[],
        })
    }

    /// The keys pm's SessionStart hook entry carries beyond its command and
    /// the Stop hook's timeout, which make the harness run it in the
    /// background once the session has started, as the waiter; `None` for
    /// a harness whose SessionStart hook does not wait.
    pub fn session_start_hook_options(self) -> Option<serde_json::Map<String, serde_json::Value>> {
        match self {
            Harness::ClaudeCode => Some(hook_options(claude_code::SESSION_START_HOOK_OPTIONS)),
            Harness::Codex | Harness::OpenCode => None,
        }
    }

    /// The keys pm's turn waiter entry under UserPromptSubmit carries
    /// beyond its command and the Stop hook's timeout, which make the
    /// harness run it in the background and wake the session with its
    /// continuation; `None` for a harness that gets no turn waiter.
    pub fn turn_waiter_hook_options(self) -> Option<serde_json::Map<String, serde_json::Value>> {
        match self {
            Harness::ClaudeCode => Some(hook_options(claude_code::STOP_HOOK_OPTIONS)),
            Harness::Codex | Harness::OpenCode => None,
        }
    }

    /// The event pm's session-end hook runs on; `None` for a harness with
    /// none that tells an exit ([`hooks_session_end`](crate::commands::hooks_session_end)).
    pub fn session_end_event(self) -> Option<&'static str> {
        match self {
            Harness::ClaudeCode => Some(claude_code::SESSION_END_EVENT),
            Harness::Codex | Harness::OpenCode => None,
        }
    }

    /// Whether a payload of the [session-end event](Self::session_end_event)
    /// says the harness process is exiting, rather than one session giving
    /// way to another in the same process.
    pub fn session_exited(self, payload: &serde_json::Value) -> bool {
        match self {
            Harness::ClaudeCode => claude_code::session_exited(payload),
            Harness::Codex | Harness::OpenCode => false,
        }
    }

    /// The harness's trust in the hooks of its user-level file — the gate
    /// without which codex runs no hooks, silently. Every hook reads trusted
    /// for a harness without hook trust. Only a [`Probe::Fresh`] asks the
    /// harness itself, which can tell a trust outdated by a changed hook.
    pub fn hook_trust(self, home: &Path, probe: Probe) -> HookTrust {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => HookTrust(None),
            Harness::Codex => HookTrust(Some(codex::hook_trust::HookTrust::read(
                &codex::home_dir(home),
                probe == Probe::Fresh,
            ))),
        }
    }

    /// Events in the harness's hooks file whose entries the harness would
    /// silently register nothing for.
    pub fn malformed_hook_events(self, hooks_root: &serde_json::Value) -> Vec<String> {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => Vec::new(),
            Harness::Codex => codex::flat_hook_events(hooks_root),
        }
    }

    /// How a user grants the harness's hook trust, for the doctor finding,
    /// by whether `config` holds their consent to pm granting it.
    pub fn hook_trust_remedy(self, config: &HarnessConfig) -> String {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => String::new(),
            Harness::Codex => codex::hook_trust_remedy(self.may_trust_pm_hooks(config)),
        }
    }

    /// The global config key holding the user's consent to pm recording
    /// the harness's trust in pm's hooks; `None` for a harness without
    /// hook trust.
    pub fn hook_trust_consent_key(self) -> Option<&'static str> {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => None,
            Harness::Codex => Some(codex::TRUST_CONSENT_KEY),
        }
    }

    /// The user's answer at [`hook_trust_consent_key`](Self::hook_trust_consent_key)
    /// in `config`; `None` while they have given none.
    pub fn hook_trust_consent(self, config: &HarnessConfig) -> Option<bool> {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => None,
            Harness::Codex => config.codex.trust_pm_hooks,
        }
    }

    /// Whether the user consented to pm recording the harness's trust in
    /// pm's hooks.
    pub fn may_trust_pm_hooks(self, config: &HarnessConfig) -> bool {
        self.hook_trust_consent(config) == Some(true)
    }

    /// Record the harness's trust in pm's hooks at `positions` (event,
    /// entry, inner hook) of its user-level file, never in any other hook;
    /// the line to print when it trusted some. Unless `recheck`, the
    /// harness is asked only when some hook has never been trusted. `None`
    /// for a harness without hook trust, one not installed, or without the
    /// user's [consent](Self::may_trust_pm_hooks).
    pub fn trust_pm_hooks(
        self,
        config: &HarnessConfig,
        home: &Path,
        positions: &[(&str, usize, usize)],
        recheck: bool,
    ) -> crate::error::Result<Option<String>> {
        match self {
            _ if !self.may_trust_pm_hooks(config) => Ok(None),
            Harness::ClaudeCode | Harness::OpenCode => Ok(None),
            Harness::Codex => {
                codex::hook_trust::trust_pm_hooks(&codex::home_dir(home), positions, recheck)
            }
        }
    }

    /// Events pm's guard hook installs under, for a harness that keeps an
    /// agent out of a directory by hook rather than at launch; empty for
    /// the others.
    pub fn guard_events(self) -> &'static [&'static str] {
        match self {
            Harness::Codex => codex::guard::EVENTS,
            Harness::ClaudeCode | Harness::OpenCode => &[],
        }
    }

    /// The absolute paths a guard hook payload's tool call would write.
    pub fn edit_targets(self, payload: &serde_json::Value) -> Vec<PathBuf> {
        match self {
            Harness::Codex => codex::guard::edit_targets(payload),
            Harness::ClaudeCode | Harness::OpenCode => Vec::new(),
        }
    }

    /// The guard hook's stdout that refuses the tool call, with `reason`.
    pub fn guard_deny_output(self, reason: &str) -> Option<String> {
        match self {
            Harness::Codex => Some(codex::guard::deny_output(reason)),
            Harness::ClaudeCode | Harness::OpenCode => None,
        }
    }

    /// Whether pm's composed prompt reaches this harness through the
    /// SessionStart hook rather than the command line.
    pub fn injects_prompt_at_session_start(self) -> bool {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => false,
            Harness::Codex => true,
        }
    }

    /// The SessionStart hook's stdout carrying `context`; `None` for a
    /// harness that does not
    /// [inject there](Self::injects_prompt_at_session_start).
    pub fn session_start_output(self, context: &str) -> Option<String> {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => None,
            Harness::Codex => Some(codex::session_start_output(context)),
        }
    }
}

fn hook_options(options: &[(&str, bool)]) -> serde_json::Map<String, serde_json::Value> {
    options
        .iter()
        .map(|(key, value)| (key.to_string(), serde_json::Value::Bool(*value)))
        .collect()
}
