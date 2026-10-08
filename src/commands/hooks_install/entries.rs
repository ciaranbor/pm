//! What pm installs: the hook events, the markers that identify pm's entry
//! under each, and the guarded shell commands.
//!
//! A user-level hook fires in every session of that harness on the machine,
//! so each installed command is guarded on `PM_AGENT_NAME`: a non-pm session
//! exits 0 before `pm` is ever resolved, which also keeps the hook inert
//! when `pm` is not on that session's `PATH`. The guard is `[ -n … ] || exit
//! 0; exec pm …`, not `&&` — `&&` would turn a false test into an exit-1
//! hook error. `exec` so the harness's child is pm itself, which can tell a
//! signal's sender; macOS `/bin/sh` does not exec a final `-c` command. The
//! guard reads the environment the hook runs in, so it identifies a session
//! only when that is the session's own.
//!
//! The Stop hook is `pm harness hooks stop <harness>`, with the keys that
//! make the harness run it in the background once the turn has ended
//! ([`Harness::stop_hook_options`]): it waits until the agent has unread
//! messages, then wakes it with the continuation (see
//! [`crate::commands::hooks_stop`]).
//!
//! The SessionStart hook is `pm harness hooks session-start`, which captures
//! the session ID from the harness's JSON input and writes it to the agent
//! registry so dead agents can be resumed, and on codex also prints the
//! agent's composed prompt as `additionalContext`. On a harness whose
//! SessionStart hook [waits](Harness::waits_at_session_start) it then runs
//! the waiter, so its entry carries the Stop hook's timeout and
//! [`Harness::session_start_hook_options`].
//!
//! The UserPromptSubmit hook is `pm harness hooks user-prompt`, which sets a
//! blocked feature back to `wip` (see [`crate::commands::hooks_user_prompt`]).
//!
//! The status hook is `pm harness hooks waiting <harness>`, installed under
//! each event of [`Harness::waiting_events`] — a different list per
//! harness, since an event one lacks may make it reject the file — and
//! records when an agent waits on the user (see
//! [`crate::commands::hooks_waiting`]). It is installed without a matcher:
//! the handler filters by payload.
//!
//! The dialog hook is `pm harness hooks dialog <harness>`, installed under
//! each event of [`Harness::dialog_events`] in an entry of its own, beside
//! the status hook's, with the Stop hook's timeout: it blocks until a
//! dialog is answered remotely (see [`crate::commands::hooks_dialog`]).
//!
//! Entries written by older releases (the unguarded `pm harness hooks …`)
//! still match a marker, so they count as pm-owned.
//!
//! A rewake loops across turns with no cap (12 consecutive wakes verified on
//! Claude Code 2.1.289, where `stop_hook_active` then reads true), so the
//! waiter's own breaker is what stops a loop that reads nothing.

use std::path::Path;

use serde_json::{Value, json};

use crate::harness::Harness;

/// Timeout in seconds for the Stop hook: a year, so the wait for a message
/// never times out in practice. Both harnesses honour the same `timeout`
/// key and default to 600s; neither has a maximum (verified live), but `0`
/// makes Claude Code reject the file and codex clamp it to 1s, and a float
/// makes codex drop every hook, so it stays a positive integer.
pub const STOP_HOOK_TIMEOUT_SECS: u64 = 31_536_000;

/// Marker string used to identify pm-owned Stop hook entries in
/// settings.json.
pub const PM_HOOK_MARKER: &str = "pm harness hooks stop";

/// Marker string for pm-owned SessionStart hook entries.
pub const PM_SESSION_START_MARKER: &str = "pm harness hooks session-start";

pub(super) const STOP_MARKERS: &[&str] = &[PM_HOOK_MARKER];
pub(super) const SESSION_START_MARKERS: &[&str] = &[PM_SESSION_START_MARKER];

/// The event of pm's hook that resets a blocked feature. Not part of the
/// never-idle loop: without it an agent still runs and wakes.
pub const USER_PROMPT_EVENT: &str = "UserPromptSubmit";

/// Marker string for pm-owned UserPromptSubmit hook entries.
pub const PM_USER_PROMPT_MARKER: &str = "pm harness hooks user-prompt";
pub(super) const USER_PROMPT_MARKERS: &[&str] = &[PM_USER_PROMPT_MARKER];

/// Marker string for pm-owned status hook entries.
pub const PM_WAITING_MARKER: &str = "pm harness hooks waiting";
pub(super) const WAITING_MARKERS: &[&str] = &[PM_WAITING_MARKER];

/// Marker string for pm-owned dialog hook entries.
pub const PM_DIALOG_MARKER: &str = "pm harness hooks dialog";
const DIALOG_MARKERS: &[&str] = &[PM_DIALOG_MARKER];

/// The hook events every harness gets, with the command markers that
/// identify pm's entry under each.
pub(super) const LOOP_EVENTS: &[(&str, &[&str])] = &[
    ("Stop", STOP_MARKERS),
    ("SessionStart", SESSION_START_MARKERS),
    (USER_PROMPT_EVENT, USER_PROMPT_MARKERS),
];

/// The hook events pm installs for `harness`, with the command markers
/// that identify pm's entry under each.
pub fn pm_events(harness: Harness) -> Vec<(&'static str, &'static [&'static str])> {
    LOOP_EVENTS
        .iter()
        .copied()
        .chain(waiting_events(harness))
        .chain(dialog_events(harness))
        .collect()
}

/// `harness`'s dialog hook events, with their marker.
pub fn dialog_events(
    harness: Harness,
) -> impl Iterator<Item = (&'static str, &'static [&'static str])> {
    harness
        .dialog_events()
        .iter()
        .map(|event| (*event, DIALOG_MARKERS))
}

/// `harness`'s status hook events, with their marker.
pub fn waiting_events(
    harness: Harness,
) -> impl Iterator<Item = (&'static str, &'static [&'static str])> {
    harness
        .waiting_events()
        .iter()
        .map(|event| (*event, WAITING_MARKERS))
}

/// Shell prefix that makes a hook exit 0 outside pm agent sessions and
/// otherwise replaces the shell with the `pm` command that follows (see the
/// module doc).
const GUARD: &str = "[ -n \"$PM_AGENT_NAME\" ] || exit 0; exec ";

/// pm's Stop hook entry for `harness`.
pub(super) fn stop_hook_entry(harness: Harness) -> Value {
    let mut entry = json!({
        "type": "command",
        "command": stop_hook_command(harness),
        "timeout": STOP_HOOK_TIMEOUT_SECS,
    });
    if let Value::Object(fields) = &mut entry {
        fields.extend(harness.stop_hook_options());
    }
    entry
}

/// The shell command registered as `harness`'s Stop hook.
pub fn stop_hook_command(harness: Harness) -> String {
    format!("{GUARD}{PM_HOOK_MARKER} {harness}")
}

/// pm's SessionStart hook entry for `harness`.
pub(super) fn session_start_hook_entry(harness: Harness) -> Value {
    let mut entry = json!({"type": "command", "command": session_start_hook_command()});
    if let Some(options) = harness.session_start_hook_options()
        && let Value::Object(fields) = &mut entry
    {
        fields.insert("timeout".into(), STOP_HOOK_TIMEOUT_SECS.into());
        fields.extend(options);
    }
    entry
}

/// The shell command registered as the SessionStart hook.
pub fn session_start_hook_command() -> String {
    format!("{GUARD}{PM_SESSION_START_MARKER}")
}

/// The shell command registered as the UserPromptSubmit hook.
pub fn user_prompt_hook_command() -> String {
    format!("{GUARD}{PM_USER_PROMPT_MARKER}")
}

/// The shell command registered as `harness`'s status hook.
pub fn waiting_hook_command(harness: Harness) -> String {
    format!("{GUARD}{PM_WAITING_MARKER} {harness}")
}

/// The shell command registered as `harness`'s dialog hook.
pub fn dialog_hook_command(harness: Harness) -> String {
    format!("{GUARD}{PM_DIALOG_MARKER} {harness}")
}

/// Every pm entry of `harness`'s user-level file: its event, the markers
/// that identify it there, and the entry.
pub(super) fn pm_entries(harness: Harness) -> Vec<(&'static str, &'static [&'static str], Value)> {
    let command = |command: String| json!({"type": "command", "command": command});
    let mut entries = vec![
        ("Stop", STOP_MARKERS, stop_hook_entry(harness)),
        (
            "SessionStart",
            SESSION_START_MARKERS,
            session_start_hook_entry(harness),
        ),
        (
            USER_PROMPT_EVENT,
            USER_PROMPT_MARKERS,
            command(user_prompt_hook_command()),
        ),
    ];
    for (event, markers) in waiting_events(harness) {
        entries.push((event, markers, command(waiting_hook_command(harness))));
    }
    for (event, markers) in dialog_events(harness) {
        // As long as the Stop hook's: a dialog may wait on the user that long.
        entries.push((
            event,
            markers,
            json!({
                "type": "command",
                "command": dialog_hook_command(harness),
                "timeout": STOP_HOOK_TIMEOUT_SECS,
            }),
        ));
    }
    entries
}

/// What this release installs as `harness`'s never-idle loop — its hook
/// entries and plugin files — as one string that changes whenever they do.
pub fn loop_fingerprint(harness: Harness) -> String {
    let mut out = String::new();
    for (event, _, entry) in pm_entries(harness) {
        out.push_str(&format!("{event}\t{entry}\n"));
    }
    for (path, content) in harness.plugin_files(Path::new("")) {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        out.push_str(&format!(
            "{name}\t{}\n",
            crate::hash::sha256_hex(content.as_bytes())
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn installed_commands_are_inert_outside_pm_sessions() {
        // The real installed strings, run the way Claude Code runs them:
        // without PM_AGENT_NAME they exit 0 with no output and never reach
        // `pm` (PATH is emptied so a resolution attempt would fail).
        for command in [
            stop_hook_command(Harness::ClaudeCode),
            session_start_hook_command(),
            user_prompt_hook_command(),
            waiting_hook_command(Harness::ClaudeCode),
        ] {
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", &command])
                .env_remove("PM_AGENT_NAME")
                .env("PATH", "")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(out.status.success(), "{command}: {out:?}");
            assert!(out.stdout.is_empty(), "{command}: {out:?}");
            assert!(out.stderr.is_empty(), "{command}: {out:?}");
        }
    }

    #[test]
    fn installed_commands_reach_pm_inside_pm_sessions() {
        // With PM_AGENT_NAME set the guard execs `pm`, resolved from PATH —
        // here a stub that echoes its parent and arguments. Its parent is
        // this test process, so no shell sits between the harness and pm.
        let dir = tempdir().unwrap();
        let stub = dir.path().join("pm");
        crate::testing::write_executable(&stub, "#!/bin/sh\necho \"stub $PPID $*\"\n");

        for (command, args) in [
            (
                stop_hook_command(Harness::ClaudeCode),
                "harness hooks stop claude-code",
            ),
            (session_start_hook_command(), "harness hooks session-start"),
            (user_prompt_hook_command(), "harness hooks user-prompt"),
            (
                waiting_hook_command(Harness::Codex),
                "harness hooks waiting codex",
            ),
        ] {
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", &command])
                .env("PM_AGENT_NAME", "x")
                .env("PATH", dir.path())
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(out.status.success(), "{command}: {out:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                format!("stub {} {args}\n", std::process::id())
            );
        }
    }
}
