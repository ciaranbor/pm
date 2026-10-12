//! `pm harness hooks guard <harness>`: keeps an agent of any scope but
//! main's out of main's checkout on a harness that has no launch-time rule
//! for it ([`Harness::guard_events`]). Run
//! synchronously before each tool call, it prints the harness's refusal when
//! the call would write under one of them, and nothing otherwise. A path is
//! matched as written and with symlinks resolved, so either spelling of a
//! protected dir is caught.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::commands::agent_spawn::protected_dirs;
use crate::error::Result;
use crate::harness::Harness;
use crate::state::paths;

/// Run the hook; always exits 0, the refusal being its stdout.
pub fn guard(harness: Harness) -> i32 {
    if std::env::var("PM_AGENT_NAME").is_ok_and(|a| !a.is_empty()) {
        let mut input = String::new();
        if std::io::stdin().read_to_string(&mut input).is_ok()
            && let Ok(payload) = serde_json::from_str(&input)
            && let Ok(dir) = paths::agent_dir()
            && let Ok(Some(refusal)) = refusal(harness, &dir, &payload)
        {
            print!("{refusal}");
        }
    }
    0
}

/// The harness's refusal of `payload`'s tool call for the agent working in
/// `agent_dir`, when the call writes under a dir its scope protects.
fn refusal(
    harness: Harness,
    agent_dir: &Path,
    payload: &serde_json::Value,
) -> Result<Option<String>> {
    let project_root = paths::find_project_root(agent_dir)?;
    let scope = paths::resolve_scope_from(&project_root, agent_dir)?;
    let protected = protected_dirs(&project_root, &scope);
    if protected.is_empty() {
        return Ok(None);
    }
    let resolved: Vec<PathBuf> = protected.iter().map(|d| resolve(d)).collect();
    let Some(target) = harness.edit_targets(payload).into_iter().find(|target| {
        let target_resolved = resolve(target);
        protected.iter().any(|d| target.starts_with(d))
            || resolved.iter().any(|d| target_resolved.starts_with(d))
    }) else {
        return Ok(None);
    };
    let reason = format!(
        "pm: {} is in main's checkout, which an agent of '{scope}' does not edit; \
         edit your own worktree instead",
        target.display()
    );
    Ok(harness.guard_deny_output(&reason))
}

/// `path` with symlinks resolved as far as it exists; the rest, which a
/// write may be about to create, appended as written.
fn resolve(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return rest.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use serde_json::{Value, json};
    use tempfile::tempdir;

    fn patch_into(cwd: &Path, target: &Path) -> Value {
        json!({
            "hook_event_name": "PreToolUse",
            "cwd": cwd,
            "tool_name": "apply_patch",
            "tool_input": {
                "command": format!("*** Begin Patch\n*** Add File: {}\n+x\n*** End Patch", target.display()),
            },
        })
    }

    fn denied(refusal: Option<String>) -> bool {
        let Some(out) = refusal else { return false };
        let out: Value = serde_json::from_str(&out).unwrap();
        out["hookSpecificOutput"]["permissionDecision"] == "deny"
            && !out["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .is_empty()
    }

    #[test]
    fn a_feature_agent_may_patch_its_own_worktree_but_not_main() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        let feature = paths::scope_worktree(&project, "login");
        let main = paths::main_worktree(&project);

        // Relative, through `..`, and in the symlink-free spelling of a
        // tempdir path that is itself behind a symlink on macOS.
        for target in [
            PathBuf::from("../main/src/new.rs"),
            main.join("README.md"),
            resolve(&main).join("README.md"),
        ] {
            let payload = patch_into(&feature, &target);
            assert!(
                denied(refusal(Harness::Codex, &feature, &payload).unwrap()),
                "{target:?}"
            );
        }
        let own = patch_into(&feature, Path::new("src/new.rs"));
        assert_eq!(refusal(Harness::Codex, &feature, &own).unwrap(), None);
    }

    #[test]
    fn a_main_agent_may_patch_main() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        let main = paths::main_worktree(&project);
        let payload = patch_into(&main, Path::new("README.md"));
        assert_eq!(refusal(Harness::Codex, &main, &payload).unwrap(), None);
    }

    #[test]
    fn a_call_that_is_not_a_patch_passes() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        let feature = paths::scope_worktree(&project, "login");
        let shell = json!({
            "cwd": feature,
            "tool_name": "Bash",
            "tool_input": {"command": "echo hi > ../main/x"},
        });
        assert_eq!(refusal(Harness::Codex, &feature, &shell).unwrap(), None);
    }
}
