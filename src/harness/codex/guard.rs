//! What pm's guard hook reads from codex, which has no path-scoped deny
//! rule: no sandbox profile can leave one directory read-only without
//! forcing Seatbelt, which cuts the agent off from tmux (see the parent
//! module). A synchronous PreToolUse hook runs before every tool call,
//! under `danger-full-access` and approval `never` too, and its deny stops
//! the call (0.160 source: `hooks/src/events/pre_tool_use.rs`). An async
//! one cannot block.
//!
//! Only apply_patch is guarded: its payload is `tool_name: "apply_patch"`
//! with the raw patch text as `tool_input.command`, whose `*** Add File:`,
//! `*** Update File:`, `*** Delete File:` and `*** Move to:` lines name
//! every path it writes, relative ones against the payload's `cwd`. A shell
//! command's writes are not.

use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

pub(in crate::harness) const EVENTS: &[&str] = &["PreToolUse"];

const PATCH_TOOL: &str = "apply_patch";

const TARGET_PREFIXES: &[&str] = &[
    "*** Add File: ",
    "*** Update File: ",
    "*** Delete File: ",
    "*** Move to: ",
];

/// Every path an apply_patch payload writes, absolute; empty for any other
/// tool call.
pub(in crate::harness) fn edit_targets(payload: &Value) -> Vec<PathBuf> {
    if payload.get("tool_name").and_then(Value::as_str) != Some(PATCH_TOOL) {
        return Vec::new();
    }
    let Some(patch) = payload
        .get("tool_input")
        .and_then(|input| input.get("command"))
        .and_then(Value::as_str)
    else {
        return Vec::new();
    };
    let cwd = Path::new(payload.get("cwd").and_then(Value::as_str).unwrap_or("/"));
    patch
        .lines()
        .filter_map(|line| {
            TARGET_PREFIXES
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        })
        .map(|target| normalize(&cwd.join(target.trim())))
        .collect()
}

/// `path` with `.` and `..` resolved lexically, as codex resolves a patch's.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The PreToolUse stdout that denies the call. Codex ignores a deny whose
/// reason is empty.
pub(in crate::harness) fn deny_output(reason: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch_payload(patch: &str) -> Value {
        json!({
            "hook_event_name": "PreToolUse",
            "cwd": "/proj/feat",
            "tool_name": "apply_patch",
            "tool_input": {"command": patch},
        })
    }

    #[test]
    fn a_patch_names_every_path_it_writes() {
        let patch = "*** Begin Patch\n\
                     *** Add File: new.txt\n\
                     +hi\n\
                     *** Update File: ../main/src/lib.rs\n\
                     *** Move to: /elsewhere/lib.rs\n\
                     @@\n\
                     -a\n\
                     +b\n\
                     *** Delete File: ./old.txt\n\
                     *** End Patch";
        assert_eq!(
            edit_targets(&patch_payload(patch)),
            [
                PathBuf::from("/proj/feat/new.txt"),
                PathBuf::from("/proj/main/src/lib.rs"),
                PathBuf::from("/elsewhere/lib.rs"),
                PathBuf::from("/proj/feat/old.txt"),
            ]
        );
    }
}
