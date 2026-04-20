//! `pm claude hooks session-start` — the SessionStart hook handler.
//!
//! Called by Claude Code when a session starts (or on compaction/clear).
//! Reads JSON from stdin, extracts the `session_id`, and writes it to
//! the agent registry so that dead agents can be resumed later.
//!
//! Non-agent sessions (no `PM_AGENT_NAME` env var) are silently ignored.

use std::io::Read;

use crate::state::agent::AgentRegistry;
use crate::state::paths;

/// Run the SessionStart hook logic. Returns the exit code (always 0).
///
/// Prints nothing on success — SessionStart hooks should not produce
/// output unless injecting context.
pub fn session_start() -> i32 {
    // Non-agent sessions: silently succeed.
    if std::env::var("PM_AGENT_NAME").is_err() {
        return 0;
    }
    match session_start_inner() {
        Ok(()) => 0,
        Err(_) => {
            // Resolution failed — not a pm project, or malformed input.
            // Don't error out; hooks should be invisible to non-pm sessions.
            0
        }
    }
}

fn session_start_inner() -> crate::error::Result<()> {
    let agent_name = std::env::var("PM_AGENT_NAME")
        .map_err(|_| crate::error::PmError::Messaging("no PM_AGENT_NAME".into()))?;

    let session_id = read_session_id_from_stdin()?;

    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let feature = resolve_feature_or_scope(&project_root, &cwd)?;

    update_agent_session_id(&project_root, &feature, &agent_name, &session_id)
}

/// Read stdin and extract `session_id` from the JSON payload.
fn read_session_id_from_stdin() -> crate::error::Result<String> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    parse_session_id(&input)
}

/// Parse `session_id` from a JSON string.
fn parse_session_id(json_str: &str) -> crate::error::Result<String> {
    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| crate::error::PmError::Messaging(format!("invalid JSON from stdin: {e}")))?;

    let session_id = parsed
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            crate::error::PmError::Messaging("missing or non-string session_id in input".into())
        })?;

    if session_id.is_empty() {
        return Err(crate::error::PmError::Messaging(
            "empty session_id in input".into(),
        ));
    }

    Ok(session_id.to_string())
}

/// Resolve the current scope: feature name from CWD, or "main".
fn resolve_feature_or_scope(
    project_root: &std::path::Path,
    cwd: &std::path::Path,
) -> crate::error::Result<String> {
    if let Some(feature) = paths::detect_feature_from_cwd(project_root, cwd) {
        return Ok(feature);
    }
    if paths::is_in_main_worktree(project_root, cwd) {
        return Ok("main".to_string());
    }
    Err(crate::error::PmError::NotInWorktree)
}

/// Update the agent's session_id in the registry.
fn update_agent_session_id(
    project_root: &std::path::Path,
    feature: &str,
    agent_name: &str,
    session_id: &str,
) -> crate::error::Result<()> {
    let agents_dir = paths::agents_dir(project_root);
    let mut registry = AgentRegistry::load(&agents_dir, feature)?;

    if let Some(entry) = registry.get_mut(agent_name) {
        entry.session_id = session_id.to_string();
        registry.save(&agents_dir, feature)?;
    }
    // If the agent isn't registered yet, silently do nothing.
    // The spawn creates the registry entry first; the hook fires after.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::agent::{AgentEntry, AgentType};
    use tempfile::tempdir;

    fn setup_project_with_agent(
        dir: &std::path::Path,
        feature: &str,
        agent_name: &str,
    ) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        std::fs::write(root.join(format!(".pm/features/{feature}.toml")), "").unwrap();

        let agents_dir = root.join(".pm/agents");
        let mut registry = AgentRegistry::default();
        registry.register(
            agent_name,
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: agent_name.to_string(),
            },
        );
        registry.save(&agents_dir, feature).unwrap();

        root
    }

    #[test]
    fn parse_session_id_from_valid_json() {
        let json = r#"{"session_id":"abc123","cwd":"/tmp","hook_event_name":"SessionStart"}"#;
        let id = parse_session_id(json).unwrap();
        assert_eq!(id, "abc123");
    }

    #[test]
    fn parse_session_id_missing_field() {
        let json = r#"{"cwd":"/tmp"}"#;
        assert!(parse_session_id(json).is_err());
    }

    #[test]
    fn parse_session_id_empty_string() {
        let json = r#"{"session_id":""}"#;
        assert!(parse_session_id(json).is_err());
    }

    #[test]
    fn parse_session_id_invalid_json() {
        assert!(parse_session_id("not json").is_err());
    }

    #[test]
    fn update_agent_session_id_writes_registry() {
        let dir = tempdir().unwrap();
        let root = setup_project_with_agent(dir.path(), "login", "reviewer");

        update_agent_session_id(&root, "login", "reviewer", "sess-42").unwrap();

        let agents_dir = root.join(".pm/agents");
        let registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        assert_eq!(registry.get("reviewer").unwrap().session_id, "sess-42");
    }

    #[test]
    fn update_agent_session_id_idempotent() {
        let dir = tempdir().unwrap();
        let root = setup_project_with_agent(dir.path(), "login", "reviewer");

        update_agent_session_id(&root, "login", "reviewer", "sess-42").unwrap();
        update_agent_session_id(&root, "login", "reviewer", "sess-42").unwrap();

        let agents_dir = root.join(".pm/agents");
        let registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        assert_eq!(registry.get("reviewer").unwrap().session_id, "sess-42");
    }

    #[test]
    fn update_agent_session_id_unknown_agent_is_noop() {
        let dir = tempdir().unwrap();
        let root = setup_project_with_agent(dir.path(), "login", "reviewer");

        // Should not error for unknown agent
        update_agent_session_id(&root, "login", "unknown-agent", "sess-42").unwrap();

        // Original agent unchanged
        let agents_dir = root.join(".pm/agents");
        let registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        assert_eq!(registry.get("reviewer").unwrap().session_id, "");
    }
}
