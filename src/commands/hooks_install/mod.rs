//! Install pm hooks into the user-level hooks file of every supported
//! harness (`~/.claude/settings.json`, `$CODEX_HOME/hooks.json`
//! — both take the same nested `hooks` shape) — once per machine. A
//! harness whose loop is a plugin gets [`Harness::plugin_files`] written
//! instead. The install is idempotent, so `pm init`, `pm update`,
//! `pm harness hooks install` and `pm doctor --fix` all run it
//! unconditionally.
//!
//! Every supported harness, not only those in use, creating `$CODEX_HOME`
//! if absent: which harnesses are in use is a per-project answer and the
//! install also runs outside any project, so a harness named only in some
//! project's `.pm/config.toml` would otherwise be skipped and its agents
//! would idle silently. The accepted cost is codex's one-time trust prompt
//! in whichever codex session comes first, pm-spawned or not. `pm doctor`
//! checks only the harnesses the project's agents run on.
//!
//! Entries written by older releases are rewritten in place in the user
//! file (see `entries` for how they are recognised).

mod check;
mod entries;
mod install;
mod settings;

pub use check::{
    hooks_registered, install_location, missing_dialog_hooks, missing_guard_hooks,
    missing_status_hooks, stale_loop_entries, stale_plugin_files,
};
#[cfg(test)]
pub(crate) use check::{is_installed_for, is_installed_in};
pub use entries::{
    PM_DIALOG_MARKER, PM_GUARD_MARKER, PM_HOOK_MARKER, PM_SESSION_START_MARKER,
    PM_USER_PROMPT_MARKER, PM_WAITING_MARKER, STOP_HOOK_TIMEOUT_SECS, USER_PROMPT_EVENT,
    dialog_events, dialog_hook_command, loop_fingerprint, pm_events, session_end_hook_command,
    session_start_hook_command, stop_hook_command, user_prompt_hook_command, waiting_events,
    waiting_hook_command,
};
pub(crate) use install::install_in;
pub use install::{install, install_dry_run};
pub use settings::{pm_hook_position, user_hooks_root};

#[cfg(doc)]
use crate::harness::Harness;

/// Helpers the submodules' tests share.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::error::Result;
    use crate::harness::Harness;
    use serde_json::Value;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::{TempDir, tempdir};

    /// An isolated home under a tempdir.
    pub(crate) fn setup() -> (TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        (dir, home)
    }

    pub(crate) fn user_file(home: &Path) -> PathBuf {
        home.join(".claude/settings.json")
    }

    pub(crate) fn codex_file(home: &Path) -> PathBuf {
        home.join(".codex/hooks.json")
    }

    pub(crate) fn plugin_dir(home: &Path) -> PathBuf {
        home.join(".config/opencode/plugins/pm-never-idle")
    }

    pub(crate) fn claude_installed_in(home: &Path) -> Result<bool> {
        super::is_installed_in(Harness::ClaudeCode, home)
    }

    pub(crate) fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    pub(crate) fn write_json(path: &Path, value: &Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    }

    pub(crate) fn command_at(root: &Value, pointer: &str) -> String {
        root.pointer(pointer)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("no string at {pointer} in {root}"))
            .to_string()
    }
}
