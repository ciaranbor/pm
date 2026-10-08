//! Whether what is installed matches what this release installs: the hook
//! entries of a harness's user-level file and its plugin files.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::entries::{LOOP_EVENTS, dialog_events, pm_entries, waiting_events};
use super::settings::pm_hook_position;
use crate::harness::Harness;
#[cfg(test)]
use crate::{error::Result, state::paths};

/// Where `harness`'s never-idle loop is installed, for messages: its hooks
/// file, else the directory of its plugin.
pub fn install_location(harness: Harness, home: &Path) -> Option<PathBuf> {
    harness.user_settings_file(home).or_else(|| {
        let (first, _) = harness.plugin_files(home).into_iter().next()?;
        first.parent().map(Path::to_path_buf)
    })
}

/// The plugin files of `harness` that are missing or differ from the
/// bundled content.
pub fn stale_plugin_files(harness: Harness, home: &Path) -> Vec<PathBuf> {
    harness
        .plugin_files(home)
        .into_iter()
        .filter(|(path, content)| {
            std::fs::read_to_string(path).map_or(true, |found| found != *content)
        })
        .map(|(path, _)| path)
        .collect()
}

/// The events of `harness`'s never-idle loop whose pm entry in `root` is
/// missing or not the one this release installs. One an earlier release
/// wrote still runs, but may not start the waiter: a SessionStart entry
/// without its options leaves a Claude Code agent spawned with no launch
/// prompt idle until a message is typed to it.
pub fn stale_loop_entries(harness: Harness, root: &Value) -> Vec<&'static str> {
    pm_entries(harness)
        .into_iter()
        .filter(|(event, _, _)| LOOP_EVENTS.iter().any(|(e, _)| e == event))
        .filter(|(event, markers, entry)| {
            pm_hook_position(root, event, markers).is_none_or(|(i, j)| {
                root.pointer(&format!("/hooks/{event}/{i}/hooks/{j}")) != Some(entry)
            })
        })
        .map(|(event, _, _)| event)
        .collect()
}

/// Whether `harness`'s never-idle loop is installed: every pm entry in its
/// user-level file, or its plugin files current.
#[cfg(test)]
pub(crate) fn is_installed_for(harness: Harness) -> Result<bool> {
    is_installed_in(harness, &paths::home_dir()?)
}

/// [`is_installed_for`] against an explicit `home`.
#[cfg(test)]
pub(crate) fn is_installed_in(harness: Harness, home: &Path) -> Result<bool> {
    Ok(stale_plugin_files(harness, home).is_empty()
        && hooks_registered(
            harness,
            home,
            super::settings::user_hooks_root(harness, home)?.as_ref(),
        ))
}

/// Whether every hook of pm's never-idle loop is registered in `root`,
/// `harness`'s parsed user hooks file under `home` (see
/// [`user_hooks_root`](super::user_hooks_root)). True for a harness with no
/// hooks file: its loop is a plugin.
pub fn hooks_registered(harness: Harness, home: &Path, root: Option<&Value>) -> bool {
    if harness.user_settings_file(home).is_none() {
        return true;
    }
    root.is_some_and(|root| {
        LOOP_EVENTS
            .iter()
            .all(|(event, markers)| pm_hook_position(root, event, markers).is_some())
    })
}

/// `harness`'s status hook events with no pm entry in `root`. The status
/// hooks are optional: without them an agent still runs and wakes.
pub fn missing_status_hooks(harness: Harness, root: Option<&Value>) -> Vec<&'static str> {
    waiting_events(harness)
        .filter(|(event, markers)| {
            root.is_none_or(|root| pm_hook_position(root, event, markers).is_none())
        })
        .map(|(event, _)| event)
        .collect()
}

/// `harness`'s dialog hook events with no pm entry in `root`. Optional
/// too: without them a dialog is answered at the terminal only.
pub fn missing_dialog_hooks(harness: Harness, root: Option<&Value>) -> Vec<&'static str> {
    dialog_events(harness)
        .filter(|(event, markers)| {
            root.is_none_or(|root| pm_hook_position(root, event, markers).is_none())
        })
        .map(|(event, _)| event)
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::commands::hooks_install::test_support::*;
    use serde_json::json;

    #[test]
    fn is_installed_false_with_foreign_stop_hook_only() {
        let (_dir, home, _root) = setup();
        assert!(!claude_installed_in(&home).unwrap());
        write_json(
            &user_file(&home),
            &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "echo foreign"}]}]}}),
        );
        assert!(!claude_installed_in(&home).unwrap());
    }
}
