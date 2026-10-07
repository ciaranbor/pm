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
        match self {
            Harness::ClaudeCode => claude_code::STOP_HOOK_OPTIONS,
            Harness::Codex => codex::STOP_HOOK_OPTIONS,
            Harness::OpenCode => &[],
        }
        .iter()
        .map(|(key, value)| (key.to_string(), serde_json::Value::Bool(*value)))
        .collect()
    }

    /// The harness's trust in the hooks of its user-level file — the gate
    /// without which codex runs no hooks, silently. Every hook reads trusted
    /// for a harness without hook trust, and when `config` has pm launch the
    /// harness with the gate bypassed. Only a [`Probe::Fresh`] asks the
    /// harness itself, which can tell a trust outdated by a changed hook.
    pub fn hook_trust(self, config: &HarnessConfig, home: &Path, probe: Probe) -> HookTrust {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => HookTrust(None),
            Harness::Codex if config.codex.bypass_hook_trust == Some(true) => HookTrust(None),
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

    /// How a user grants the harness's hook trust, for the doctor finding.
    pub fn hook_trust_remedy(self) -> &'static str {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => "",
            Harness::Codex => codex::HOOK_TRUST_REMEDY,
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
