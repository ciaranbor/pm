//! Install the pm Stop hook into `main/.claude/settings.json`.
//!
//! The Stop hook is `pm hooks stop`, a Rust command that checks the agent's
//! inbox and the waiting lock file to decide whether Claude should stop:
//!
//! - **Unread messages** → `block` + "read your messages"
//! - **No unread, no background wait** → `block` + "start `pm msg wait` in
//!   background"
//! - **No unread, background wait running** → `allow` — the background
//!   `pm msg wait` will wake Claude via task-notification when a message
//!   arrives.
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

/// Marker string used to identify pm-owned Stop hook entries in
/// settings.json. Present in both the old `printf` command and the new
/// `pm hooks stop` command so upgrades detect and replace either.
pub const PM_HOOK_MARKER: &str = "pm hooks stop";

/// The shell command registered as the Stop hook. Invokes `pm hooks stop`
/// which checks the inbox and lock file, printing the appropriate JSON
/// decision to stdout.
pub fn stop_hook_command() -> String {
    "pm hooks stop".to_string()
}

/// Install the Stop hook into `main/.claude/settings.json`. Idempotent:
/// re-running overwrites an existing pm-owned entry with matching command,
/// leaving foreign Stop hook entries alone (append-only).
///
/// Returns a human-readable status line describing what happened.
pub fn install(project_root: &Path) -> Result<String> {
    let main_claude_dir = project_root.join("main").join(".claude");
    let settings_path = main_claude_dir.join("settings.json");

    let mut root = load_or_init_settings(&settings_path)?;
    let changed = upsert_stop_hook(&mut root)?;

    if changed {
        if !main_claude_dir.exists() {
            std::fs::create_dir_all(&main_claude_dir)?;
        }
        let serialized = serde_json::to_string_pretty(&root)
            .map_err(|e| PmError::Io(std::io::Error::other(e.to_string())))?;
        std::fs::write(&settings_path, format!("{serialized}\n"))?;
        Ok(format!(
            "Installed pm Stop hook in {}",
            settings_path.display()
        ))
    } else {
        Ok(format!(
            "pm Stop hook already installed in {}",
            settings_path.display()
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
/// `pm hooks stop` command and the old `printf` approach (which contained
/// our marker text in the reason string). Foreign entries are left alone.
fn entry_is_pm_owned(entry: &Value) -> bool {
    let Some(inner) = entry.get("hooks").and_then(|v| v.as_array()) else {
        return false;
    };
    inner.iter().any(|hook| {
        hook.get("command")
            .and_then(|v| v.as_str())
            .is_some_and(|cmd| cmd.contains(PM_HOOK_MARKER) || cmd.contains("pm msg wait"))
    })
}

/// Check whether the Stop hook is installed in `main/.claude/settings.json`.
/// Used by `pm doctor`. Returns `Ok(true)` when a pm-owned entry is present,
/// `Ok(false)` otherwise (including when the file does not exist).
pub fn is_installed(project_root: &Path) -> Result<bool> {
    let path = project_root
        .join("main")
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
    let Some(stop_array) = parsed
        .get("hooks")
        .and_then(|v| v.get("Stop"))
        .and_then(|v| v.as_array())
    else {
        return Ok(false);
    };
    Ok(stop_array.iter().any(entry_is_pm_owned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_project(dir: &Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join("main")).unwrap();
        root
    }

    #[test]
    fn install_creates_settings_when_missing() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let msg = install(&root).unwrap();
        assert!(msg.contains("Installed"));

        let path = root.join("main").join(".claude").join("settings.json");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
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
        assert_eq!(cmd, "pm hooks stop");
    }

    #[test]
    fn install_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        install(&root).unwrap();
        let path = root.join("main").join(".claude").join("settings.json");
        let first = std::fs::read_to_string(&path).unwrap();

        let msg = install(&root).unwrap();
        assert!(msg.contains("already installed"));
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn install_preserves_other_top_level_keys() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let claude_dir = root.join("main").join(".claude");
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

        let claude_dir = root.join("main").join(".claude");
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
        // `pm hooks stop`.
        let claude_dir = root.join("main").join(".claude");
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
        assert_eq!(cmd, "pm hooks stop");
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
        let claude_dir = root.join("main").join(".claude");
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
        assert_eq!(stop_hook_command(), "pm hooks stop");
    }
}
