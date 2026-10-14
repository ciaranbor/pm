//! Reading and editing a hooks file's nested `hooks` shape: upserting pm's
//! entry under an event, and locating one.
//!
//! Only pm's inner hook is ever replaced, so a foreign hook the
//! user bundled into the same entry survives. Codex runs no hook until it
//! is trusted (`pm doctor` reports a missing trust entry) and keys that
//! trust on the entry's index, so pm appends its entries and existing ones
//! keep their positions.

use std::path::Path;

use serde_json::{Value, json};

use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::harness::Harness;

/// Parse a settings file. `None` when it is missing or empty; an error when
/// it isn't a JSON object (never clobber a file we don't understand).
pub(super) fn load_settings(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    let parsed: Value = serde_json::from_str(&content).map_err(|e| {
        PmError::Io(std::io::Error::other(format!(
            "{}: invalid JSON: {e}",
            path.display()
        )))
    })?;
    if !parsed.is_object() {
        return Err(PmError::Io(std::io::Error::other(format!(
            "{}: root must be a JSON object",
            path.display()
        ))));
    }
    Ok(Some(parsed))
}

pub(super) fn write_settings(path: &Path, root: &Value) -> Result<()> {
    let serialized = serde_json::to_string_pretty(root)
        .map_err(|e| PmError::Io(std::io::Error::other(e.to_string())))?;
    write_atomic(path, format!("{serialized}\n").as_bytes())
}

/// `hooks.<event>` as a mutable array, creating it when absent. Errors when
/// `hooks` or the event slot holds something other than the expected shape.
fn event_array<'a>(root: &'a mut Value, event: &str) -> Result<&'a mut Vec<Value>> {
    let obj = root.as_object_mut().expect("validated in load_settings");
    let hooks_entry = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !hooks_entry.is_object() {
        return Err(PmError::Io(std::io::Error::other(
            "settings.json `hooks` must be an object",
        )));
    }
    let entry = hooks_entry
        .as_object_mut()
        .unwrap()
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    entry.as_array_mut().ok_or_else(|| {
        PmError::Io(std::io::Error::other(format!(
            "settings.json `hooks.{event}` must be an array"
        )))
    })
}

/// Replace the pm-owned inner hook in `hooks.<event>` (any generation) with
/// `pm_hook` in place — so a foreign hook the user bundled into the same
/// entry survives — or append a new entry holding it. Returns `true` when
/// the value changed.
pub(super) fn upsert_hook(
    root: &mut Value,
    event: &str,
    markers: &[&str],
    pm_hook: Value,
) -> Result<bool> {
    let array = event_array(root, event)?;
    for entry in array.iter_mut() {
        let Some(inner) = entry.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        if let Some(idx) = inner.iter().position(|h| command_matches(h, markers)) {
            if inner[idx] == pm_hook {
                return Ok(false);
            }
            inner[idx] = pm_hook;
            return Ok(true);
        }
    }
    array.push(json!({ "hooks": [pm_hook] }));
    Ok(true)
}

fn command_matches(hook: &Value, markers: &[&str]) -> bool {
    hook.get("command")
        .and_then(|v| v.as_str())
        .is_some_and(|cmd| markers.iter().any(|m| cmd.contains(m)))
}

/// The parsed user-level hooks file of `harness`; `None` when it has none,
/// or the file is missing or not JSON.
pub fn user_hooks_root(harness: Harness, home: &Path) -> Result<Option<Value>> {
    let Some(path) = harness.user_settings_file(home) else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str::<Value>(&content).ok())
}

/// Where each of `harness`'s pm hooks sits in `root`, as (event, entry,
/// inner hook); a hook not installed is left out.
pub fn pm_hook_positions(harness: Harness, root: &Value) -> Vec<(&'static str, usize, usize)> {
    super::pm_events(harness)
        .into_iter()
        .filter_map(|(event, markers)| {
            pm_hook_position(root, event, markers).map(|(entry, hook)| (event, entry, hook))
        })
        .collect()
}

/// Where pm's hook sits under `hooks.<event>`: the entry index and the
/// inner-hook index — the coordinates codex keys hook trust on.
pub fn pm_hook_position(root: &Value, event: &str, markers: &[&str]) -> Option<(usize, usize)> {
    let entries = root.get("hooks")?.get(event)?.as_array()?;
    entries.iter().enumerate().find_map(|(i, entry)| {
        let inner = entry.get("hooks")?.as_array()?;
        let j = inner.iter().position(|h| command_matches(h, markers))?;
        Some((i, j))
    })
}
