//! `pm harness hooks session-start` — the SessionStart hook handler.
//!
//! Called by the harness when a session starts, resumes, or is compacted.
//! Reads JSON from stdin, extracts the `session_id`, and writes it to
//! the agent registry so that dead agents can be resumed later. For a
//! harness with no launch-time role channel (codex), it also prints the
//! agent's composed prompt — definition body, baseline, notice boards — as
//! `additionalContext`, so the role re-applies on every start and resume.
//!
//! Non-agent sessions (no `PM_AGENT_NAME` env var) are silently ignored.

use std::io::Read;
use std::path::Path;

use crate::error::Result;
use crate::harness::Harness;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::workflow;

/// Run the SessionStart hook logic. Returns the exit code (always 0).
///
/// Prints nothing on success unless the agent's harness takes its prompt
/// through this hook.
pub fn session_start() -> i32 {
    // Non-agent sessions: silently succeed.
    if std::env::var("PM_AGENT_NAME").is_err() {
        return 0;
    }
    match session_start_inner() {
        Ok(Some(output)) => {
            print!("{output}");
            0
        }
        Ok(None) => 0,
        Err(_) => {
            // Resolution failed — not a pm project, or malformed input.
            // Don't error out; hooks should be invisible to non-pm sessions.
            0
        }
    }
}

fn session_start_inner() -> Result<Option<String>> {
    let agent_name = std::env::var("PM_AGENT_NAME")
        .map_err(|_| crate::error::PmError::Messaging("no PM_AGENT_NAME".into()))?;

    let session_id = read_session_id_from_stdin()?;

    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let feature = paths::resolve_scope_from(&project_root, &cwd)?;

    let Some((harness, definition)) =
        update_agent_session_id(&project_root, &feature, &agent_name, &session_id)?
    else {
        return Ok(None);
    };
    hook_output(&project_root, harness, &definition)
}

/// What the hook prints for this agent's harness: the composed prompt for
/// a harness that injects it here, nothing otherwise.
fn hook_output(project_root: &Path, harness: Harness, definition: &str) -> Result<Option<String>> {
    if !harness.injects_prompt_at_session_start() {
        return Ok(None);
    }
    let context = injected_context(project_root, definition)?;
    Ok(context.and_then(|c| harness.session_start_output(&c)))
}

/// The agent's definition body (unless it is the vanilla agent) followed by
/// the baseline-and-notices text — what Claude Code gets from `--agent` and
/// `--append-system-prompt-file`. `None` when neither exists.
fn injected_context(project_root: &Path, definition: &str) -> Result<Option<String>> {
    let home = paths::home_dir().ok();
    let body = if workflow::is_vanilla(definition) {
        None
    } else {
        workflow::definition_body(project_root, definition, home.as_deref())?
    };
    let baseline = crate::notice::compose_spawn_prompt_text(project_root)?;
    Ok(match (body, baseline) {
        (None, None) => None,
        (Some(b), None) => Some(b),
        (None, Some(t)) => Some(t),
        (Some(b), Some(t)) => Some(format!("{}\n\n{t}", b.trim_end())),
    })
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

/// Update the agent's session_id in the registry, returning the harness and
/// effective definition its entry records. An unregistered agent is left
/// alone (`None`): the spawn registers before launching, so this is a
/// non-pm session.
fn update_agent_session_id(
    project_root: &Path,
    feature: &str,
    agent_name: &str,
    session_id: &str,
) -> Result<Option<(Harness, String)>> {
    let agents_dir = paths::agents_dir(project_root);
    let mut registry = AgentRegistry::load(&agents_dir, feature)?;

    let Some(entry) = registry.get_mut(agent_name) else {
        return Ok(None);
    };
    entry.session_id = session_id.to_string();
    let recorded = (
        entry.harness,
        entry.effective_definition(agent_name).to_string(),
    );
    registry.save(&agents_dir, feature)?;
    Ok(Some(recorded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Harness;
    use crate::state::agent::{AgentEntry, AgentType};
    use tempfile::tempdir;

    fn setup_project_with_agent(
        dir: &std::path::Path,
        feature: &str,
        agent_name: &str,
    ) -> std::path::PathBuf {
        setup_project_with_agent_on(dir, feature, agent_name, None, Harness::ClaudeCode)
    }

    fn setup_project_with_agent_on(
        dir: &std::path::Path,
        feature: &str,
        agent_name: &str,
        agent_definition: Option<&str>,
        harness: Harness,
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
                active: true,
                agent_definition: agent_definition.map(String::from),
                harness,
            },
        );
        registry.save(&agents_dir, feature).unwrap();

        root
    }

    fn write_project_def(root: &std::path::Path, name: &str, body: &str) {
        let dir = paths::main_worktree(root).join(".agents/agents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.md")), body).unwrap();
    }

    fn additional_context(output: &str) -> String {
        let parsed: serde_json::Value = serde_json::from_str(output).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"], "SessionStart",
            "{output}"
        );
        parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn claude_code_agent_prints_nothing() {
        let dir = tempdir().unwrap();
        let root = setup_project_with_agent(dir.path(), "login", "reviewer");
        write_project_def(&root, "reviewer", "# Reviewer");
        let (harness, definition) = update_agent_session_id(&root, "login", "reviewer", "s1")
            .unwrap()
            .unwrap();
        assert_eq!(
            (harness, definition.as_str()),
            (Harness::ClaudeCode, "reviewer")
        );
        assert_eq!(hook_output(&root, harness, &definition).unwrap(), None);
        // An unregistered agent is not a pm agent at all.
        assert_eq!(
            update_agent_session_id(&root, "login", "ghost", "s1").unwrap(),
            None
        );
    }

    #[test]
    fn codex_agent_gets_its_definition_body_and_baseline_as_context() {
        crate::commands::skills::install_global().unwrap();
        let baseline =
            std::fs::read_to_string(paths::home_dir().unwrap().join(".agents/pm-baseline.md"))
                .unwrap();
        let dir = tempdir().unwrap();
        // A named agent: the alias `backend` runs the `implementer` definition.
        let root = setup_project_with_agent_on(
            dir.path(),
            "login",
            "backend",
            Some("implementer"),
            Harness::Codex,
        );
        write_project_def(
            &root,
            "implementer",
            "---\nname: implementer\n---\n# Implementer\n\nBuild things.\n",
        );

        let (harness, definition) = update_agent_session_id(&root, "login", "backend", "s1")
            .unwrap()
            .unwrap();
        assert_eq!(
            (harness, definition.as_str()),
            (Harness::Codex, "implementer")
        );
        let out = hook_output(&root, harness, &definition)
            .unwrap()
            .expect("codex agents get context");
        assert_eq!(
            additional_context(&out),
            format!("# Implementer\n\nBuild things.\n\n{baseline}")
        );

        // With a project notice board the same composed text Claude Code
        // gets as a prompt file follows the definition body.
        std::fs::write(root.join(".pm/notices.md"), "Never force-push.\n").unwrap();
        let composed = crate::notice::compose_spawn_prompt_text(&root)
            .unwrap()
            .unwrap();
        assert_ne!(composed, baseline);
        assert!(composed.starts_with(baseline.trim_end()), "{composed}");
        assert!(
            composed.ends_with("# Notice board — project\nNever force-push.\n"),
            "{composed}"
        );
        let out = hook_output(&root, harness, &definition).unwrap().unwrap();
        assert_eq!(
            additional_context(&out),
            format!("# Implementer\n\nBuild things.\n\n{composed}")
        );
    }

    #[test]
    fn codex_vanilla_agent_gets_the_baseline_only() {
        crate::commands::skills::install_global().unwrap();
        let baseline =
            std::fs::read_to_string(paths::home_dir().unwrap().join(".agents/pm-baseline.md"))
                .unwrap();
        let dir = tempdir().unwrap();
        let root =
            setup_project_with_agent_on(dir.path(), "login", "default", None, Harness::Codex);
        // Even a same-named definition file is ignored for the vanilla agent.
        write_project_def(&root, "default", "# should not appear");
        let out = hook_output(&root, Harness::Codex, "default")
            .unwrap()
            .unwrap();
        assert_eq!(additional_context(&out), baseline);
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
