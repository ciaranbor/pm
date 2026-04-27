//! Install pm hooks (Stop + SessionStart) into `main/.claude/settings.json`.
//!
//! The Stop hook is `pm claude hooks stop`, a Rust command that blocks until
//! the agent has unread messages (by calling `agent_wait` internally),
//! then returns `{"decision":"block","reason":"You have new messages…"}`.
//! Claude Code delivers the reason as a continuation prompt, the agent
//! reads its messages, the turn ends, and the hook fires again.
//!
//! The SessionStart hook is `pm claude hooks session-start`, which captures
//! the session ID from Claude Code's JSON input and writes it to the agent
//! registry so dead agents can be resumed with `--resume <session_id>`.
//!
//! # Stop-hook prototype
//!
//! Verified empirically (see `agents-as-message-processors` feature):
//! `{"decision":"block","reason":"..."}` loops indefinitely across real turns
//! (tested 82 consecutive turns with no hard cap). `stop_hook_active` is
//! advisory or auto-resetting.

use std::path::Path;

use serde_json::{Value, json};

use crate::error::{PmError, Result};
use crate::state::paths;

/// Timeout in seconds for the Stop hook. Claude Code's default is 600s
/// (10 minutes), which is too short for agents that block waiting for
/// messages. 24 hours gives ample headroom.
pub const STOP_HOOK_TIMEOUT_SECS: u64 = 86400;

/// Marker string used to identify pm-owned Stop hook entries in
/// settings.json. Present in both the old `printf` command and the new
/// `pm claude hooks stop` command so upgrades detect and replace either.
pub const PM_HOOK_MARKER: &str = "pm claude hooks stop";

/// Marker string for pm-owned SessionStart hook entries.
pub const PM_SESSION_START_MARKER: &str = "pm claude hooks session-start";

/// The shell command registered as the Stop hook. Invokes `pm claude hooks stop`
/// which blocks until unread messages are available, printing the JSON
/// decision to stdout.
pub fn stop_hook_command() -> String {
    "pm claude hooks stop".to_string()
}

/// The shell command registered as the SessionStart hook.
pub fn session_start_hook_command() -> String {
    "pm claude hooks session-start".to_string()
}

/// Install pm hooks (Stop + SessionStart) into `main/.claude/settings.json`.
/// Idempotent: re-running overwrites existing pm-owned entries with matching
/// commands, leaving foreign hook entries alone (append-only).
///
/// Returns a human-readable status line describing what happened.
pub fn install(project_root: &Path) -> Result<String> {
    let (msg, _) = install_inner(project_root, false)?;
    Ok(msg)
}

/// Dry-run variant of [`install`]. Returns `Some(line)` describing the action
/// that would be taken when something would change, or `None` when pm hooks
/// are already installed and up to date.
pub fn install_dry_run(project_root: &Path) -> Result<Option<String>> {
    let (msg, would_change) = install_inner(project_root, true)?;
    Ok(if would_change { Some(msg) } else { None })
}

/// Returns the status message and whether on-disk state would (in `dry_run`
/// mode) or did (in apply mode) change.
fn install_inner(project_root: &Path, dry_run: bool) -> Result<(String, bool)> {
    let main_claude_dir = paths::main_worktree(project_root).join(".claude");
    let settings_path = main_claude_dir.join("settings.json");

    let mut root = load_or_init_settings(&settings_path)?;
    let stop_changed = upsert_stop_hook(&mut root)?;
    let session_start_changed = upsert_session_start_hook(&mut root)?;
    let changed = stop_changed || session_start_changed;

    if changed {
        if dry_run {
            return Ok((
                format!("Would install pm hooks in {}", settings_path.display()),
                true,
            ));
        }
        if !main_claude_dir.exists() {
            std::fs::create_dir_all(&main_claude_dir)?;
        }
        let serialized = serde_json::to_string_pretty(&root)
            .map_err(|e| PmError::Io(std::io::Error::other(e.to_string())))?;
        std::fs::write(&settings_path, format!("{serialized}\n"))?;
        Ok((
            format!("Installed pm hooks in {}", settings_path.display()),
            true,
        ))
    } else {
        Ok((
            format!("pm hooks already installed in {}", settings_path.display()),
            false,
        ))
    }
}

/// Load the existing settings JSON or return an empty object when the file
/// does not exist yet.
fn load_or_init_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let content = std::fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let parsed: Value = serde_json::from_str(&content)
        .map_err(|e| PmError::Io(std::io::Error::other(format!("invalid JSON: {e}"))))?;
    if !parsed.is_object() {
        return Err(PmError::Io(std::io::Error::other(
            "settings.json root must be a JSON object",
        )));
    }
    Ok(parsed)
}

/// Upsert the pm-owned Stop hook entry into a settings JSON value. Returns
/// `true` when the on-disk file would change.
fn upsert_stop_hook(root: &mut Value) -> Result<bool> {
    let command = stop_hook_command();
    let pm_hook_entry = json!({
        "hooks": [
            {
                "type": "command",
                "command": command,
                "timeout": STOP_HOOK_TIMEOUT_SECS,
            }
        ]
    });

    let obj = root.as_object_mut().expect("validated in load_or_init");
    let hooks_entry = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !hooks_entry.is_object() {
        return Err(PmError::Io(std::io::Error::other(
            "settings.json `hooks` must be an object",
        )));
    }
    let hooks_obj = hooks_entry.as_object_mut().unwrap();
    let stop_entry = hooks_obj
        .entry("Stop".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !stop_entry.is_array() {
        return Err(PmError::Io(std::io::Error::other(
            "settings.json `hooks.Stop` must be an array",
        )));
    }
    let stop_array = stop_entry.as_array_mut().unwrap();

    // Find an existing pm-owned entry: one whose inner hooks contain a command
    // that invokes printf with our canned reason text. We match on the reason
    // text so updates to the exact shell quoting still overwrite in place.
    let existing_idx = stop_array.iter().position(entry_is_pm_owned);

    if let Some(idx) = existing_idx {
        if stop_array[idx] == pm_hook_entry {
            return Ok(false);
        }
        stop_array[idx] = pm_hook_entry;
    } else {
        stop_array.push(pm_hook_entry);
    }

    Ok(true)
}

/// Heuristic: is this Stop entry one we own? Matches both the current
/// `pm claude hooks stop` command and the old `printf` approach (which contained
/// our marker text in the reason string). Foreign entries are left alone.
fn entry_is_pm_owned(entry: &Value) -> bool {
    let Some(inner) = entry.get("hooks").and_then(|v| v.as_array()) else {
        return false;
    };
    inner.iter().any(|hook| {
        hook.get("command")
            .and_then(|v| v.as_str())
            .is_some_and(|cmd| {
                cmd.contains(PM_HOOK_MARKER)
                    || cmd.contains("pm hooks stop")
                    || cmd.contains("pm msg wait")
            })
    })
}

/// Upsert the pm-owned SessionStart hook entry into a settings JSON value.
/// Returns `true` when the on-disk file would change.
fn upsert_session_start_hook(root: &mut Value) -> Result<bool> {
    let command = session_start_hook_command();
    let pm_hook_entry = json!({
        "hooks": [
            {
                "type": "command",
                "command": command,
            }
        ]
    });

    let obj = root.as_object_mut().expect("validated in load_or_init");
    let hooks_entry = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !hooks_entry.is_object() {
        return Err(PmError::Io(std::io::Error::other(
            "settings.json `hooks` must be an object",
        )));
    }
    let hooks_obj = hooks_entry.as_object_mut().unwrap();
    let ss_entry = hooks_obj
        .entry("SessionStart".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !ss_entry.is_array() {
        return Err(PmError::Io(std::io::Error::other(
            "settings.json `hooks.SessionStart` must be an array",
        )));
    }
    let ss_array = ss_entry.as_array_mut().unwrap();

    let existing_idx = ss_array.iter().position(session_start_entry_is_pm_owned);

    if let Some(idx) = existing_idx {
        if ss_array[idx] == pm_hook_entry {
            return Ok(false);
        }
        ss_array[idx] = pm_hook_entry;
    } else {
        ss_array.push(pm_hook_entry);
    }

    Ok(true)
}

/// Heuristic: is this SessionStart entry one we own?
fn session_start_entry_is_pm_owned(entry: &Value) -> bool {
    let Some(inner) = entry.get("hooks").and_then(|v| v.as_array()) else {
        return false;
    };
    inner.iter().any(|hook| {
        hook.get("command")
            .and_then(|v| v.as_str())
            .is_some_and(|cmd| cmd.contains(PM_SESSION_START_MARKER))
    })
}

/// Check whether pm hooks are installed in `main/.claude/settings.json`.
/// Used by `pm doctor`. Returns `Ok(true)` when both Stop and SessionStart
/// pm-owned entries are present, `Ok(false)` otherwise.
pub fn is_installed(project_root: &Path) -> Result<bool> {
    let path = paths::main_worktree(project_root)
        .join(".claude")
        .join("settings.json");
    if !path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&path)?;
    if content.trim().is_empty() {
        return Ok(false);
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&content) else {
        return Ok(false);
    };
    let hooks = parsed.get("hooks");

    let has_stop = hooks
        .and_then(|v| v.get("Stop"))
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(entry_is_pm_owned));

    let has_session_start = hooks
        .and_then(|v| v.get("SessionStart"))
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(session_start_entry_is_pm_owned));

    Ok(has_stop && has_session_start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_project(dir: &Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(paths::main_worktree(&root)).unwrap();
        root
    }

    #[test]
    fn install_creates_settings_when_missing() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let msg = install(&root).unwrap();
        assert!(msg.contains("Installed"));

        let path = paths::main_worktree(&root)
            .join(".claude")
            .join("settings.json");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();

        // Stop hook
        let stop = parsed
            .get("hooks")
            .and_then(|v| v.get("Stop"))
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(stop.len(), 1);
        let cmd = stop[0]
            .get("hooks")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|h| h.get("command"))
            .and_then(|c| c.as_str())
            .unwrap();
        assert_eq!(cmd, "pm claude hooks stop");

        // SessionStart hook
        let ss = parsed
            .get("hooks")
            .and_then(|v| v.get("SessionStart"))
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(ss.len(), 1);
        let ss_cmd = ss[0]
            .get("hooks")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|h| h.get("command"))
            .and_then(|c| c.as_str())
            .unwrap();
        assert_eq!(ss_cmd, "pm claude hooks session-start");
    }

    #[test]
    fn install_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        install(&root).unwrap();
        let path = paths::main_worktree(&root)
            .join(".claude")
            .join("settings.json");
        let first = std::fs::read_to_string(&path).unwrap();

        let msg = install(&root).unwrap();
        assert!(msg.contains("already installed"));
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn install_preserves_foreign_session_start_hooks() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let claude_dir = paths::main_worktree(&root).join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let existing = json!({
            "hooks": {
                "SessionStart": [
                    {
                        "hooks": [
                            { "type": "command", "command": "echo foreign session hook" }
                        ]
                    }
                ]
            }
        });
        std::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        install(&root).unwrap();

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        let ss = parsed
            .get("hooks")
            .and_then(|v| v.get("SessionStart"))
            .and_then(|v| v.as_array())
            .unwrap();
        // Foreign hook preserved + pm hook added
        assert_eq!(ss.len(), 2);
        let first_cmd = ss[0]
            .get("hooks")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|h| h.get("command"))
            .and_then(|c| c.as_str())
            .unwrap();
        assert_eq!(first_cmd, "echo foreign session hook");
    }

    #[test]
    fn install_replaces_old_session_start_hook() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // First install
        install(&root).unwrap();

        // Manually modify the SessionStart hook command to simulate an old version
        let claude_dir = paths::main_worktree(&root).join(".claude");
        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let mut parsed: Value = serde_json::from_str(&content).unwrap();
        let ss = parsed
            .pointer_mut("/hooks/SessionStart/0/hooks/0/command")
            .unwrap();
        *ss = json!("pm claude hooks session-start --old-flag");

        std::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&parsed).unwrap(),
        )
        .unwrap();

        // Re-install should replace it
        let msg = install(&root).unwrap();
        assert!(msg.contains("Installed"));

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        let ss = parsed
            .get("hooks")
            .and_then(|v| v.get("SessionStart"))
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(ss.len(), 1);
        let cmd = ss[0]
            .get("hooks")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|h| h.get("command"))
            .and_then(|c| c.as_str())
            .unwrap();
        assert_eq!(cmd, "pm claude hooks session-start");
    }

    #[test]
    fn install_preserves_other_top_level_keys() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let claude_dir = paths::main_worktree(&root).join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"model":"sonnet","permissions":{"allow":["Read"]}}"#,
        )
        .unwrap();

        install(&root).unwrap();

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.get("model").and_then(|v| v.as_str()), Some("sonnet"));
        assert!(parsed.get("permissions").is_some());
        assert!(parsed.get("hooks").is_some());
    }

    #[test]
    fn install_preserves_other_stop_hooks() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let claude_dir = paths::main_worktree(&root).join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let existing = json!({
            "hooks": {
                "Stop": [
                    {
                        "hooks": [
                            { "type": "command", "command": "echo existing user hook" }
                        ]
                    }
                ]
            }
        });
        std::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        install(&root).unwrap();

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        let stop = parsed
            .get("hooks")
            .and_then(|v| v.get("Stop"))
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(stop.len(), 2);
        // The original user hook still lives at index 0
        let first_cmd = stop[0]
            .get("hooks")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|h| h.get("command"))
            .and_then(|c| c.as_str())
            .unwrap();
        assert_eq!(first_cmd, "echo existing user hook");
    }

    #[test]
    fn install_replaces_old_printf_hook() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Seed the old-style printf hook — install should replace it with
        // `pm claude hooks stop`.
        let claude_dir = paths::main_worktree(&root).join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let old = json!({
            "hooks": {
                "Stop": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "printf '{\"decision\":\"block\",\"reason\":\"... pm msg wait ...\"}'",
                            }
                        ]
                    }
                ]
            }
        });
        std::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&old).unwrap(),
        )
        .unwrap();

        install(&root).unwrap();

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        let stop = parsed
            .get("hooks")
            .and_then(|v| v.get("Stop"))
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(stop.len(), 1);
        let cmd = stop[0]
            .get("hooks")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|h| h.get("command"))
            .and_then(|c| c.as_str())
            .unwrap();
        assert_eq!(cmd, "pm claude hooks stop");
    }

    #[test]
    fn is_installed_false_when_file_missing() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        assert!(!is_installed(&root).unwrap());
    }

    #[test]
    fn is_installed_true_after_install() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        install(&root).unwrap();
        assert!(is_installed(&root).unwrap());
    }

    #[test]
    fn is_installed_false_with_foreign_stop_hook_only() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let claude_dir = paths::main_worktree(&root).join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo foreign"}]}]}}"#,
        )
        .unwrap();
        assert!(!is_installed(&root).unwrap());
    }

    #[test]
    fn stop_hook_command_is_pm_hooks_stop() {
        assert_eq!(stop_hook_command(), "pm claude hooks stop");
    }

    #[test]
    fn stop_hook_has_timeout() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        install(&root).unwrap();

        let path = paths::main_worktree(&root)
            .join(".claude")
            .join("settings.json");
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();

        let timeout = parsed
            .pointer("/hooks/Stop/0/hooks/0/timeout")
            .and_then(|v| v.as_u64())
            .unwrap();
        assert_eq!(timeout, STOP_HOOK_TIMEOUT_SECS);
    }

    #[test]
    fn install_upgrades_hook_without_timeout() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Seed a pm-owned stop hook that lacks the timeout field (old format)
        let claude_dir = paths::main_worktree(&root).join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let old = json!({
            "hooks": {
                "Stop": [
                    {
                        "hooks": [
                            { "type": "command", "command": "pm claude hooks stop" }
                        ]
                    }
                ],
                "SessionStart": [
                    {
                        "hooks": [
                            { "type": "command", "command": "pm claude hooks session-start" }
                        ]
                    }
                ]
            }
        });
        std::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&old).unwrap(),
        )
        .unwrap();

        let msg = install(&root).unwrap();
        assert!(
            msg.contains("Installed"),
            "should detect missing timeout as a change"
        );

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        let timeout = parsed
            .pointer("/hooks/Stop/0/hooks/0/timeout")
            .and_then(|v| v.as_u64())
            .unwrap();
        assert_eq!(timeout, STOP_HOOK_TIMEOUT_SECS);
    }
}
