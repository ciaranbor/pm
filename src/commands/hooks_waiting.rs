//! `pm harness hooks waiting <harness>`: keeps the agent's waiting marker
//! ([`runtime`]) in step with what its harness reports — a dialog opening
//! or resolving, an interrupt, a failed turn — so the attention view can
//! tell an agent waiting on the user from one at work. Which events say
//! what is the harness's own knowledge ([`Harness::waiting_event`]); the
//! harness is named on the command line because the hooks file it is
//! installed in, or the plugin calling it, already knows it.
//!
//! It runs on every tool call, so the common path — a tool finished, no
//! marker — costs one stamp of the agent's activity and one failed unlink,
//! with no registry read. Its stdout can approve or deny a tool, so it
//! prints nothing and always exits 0.

use std::io::Read;
use std::path::Path;

use crate::commands::attention::AgentState;
use crate::error::Result;
use crate::harness::{Harness, WaitingEvent};
use crate::messages;
use crate::state::paths;
use crate::state::runtime;

/// Run the hook. `on_change` is told the agent's new state and unread count
/// when its marker changed.
pub fn waiting(harness: Harness, on_change: impl FnOnce(AgentState, u32)) -> i32 {
    if let Ok(Some((state, unread))) = waiting_inner(harness) {
        on_change(state, unread);
    }
    0
}

fn waiting_inner(harness: Harness) -> Result<Option<(AgentState, u32)>> {
    let Some(agent) = std::env::var("PM_AGENT_NAME")
        .ok()
        .filter(|a| !a.is_empty())
    else {
        return Ok(None);
    };
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&input) else {
        return Ok(None);
    };
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let scope = paths::resolve_scope_from(&project_root, &cwd)?;
    runtime::touch_activity(&project_root, &scope, &agent)?;
    let Some(state) = apply(
        &project_root,
        &scope,
        &agent,
        harness.waiting_event(&payload),
    )?
    else {
        return Ok(None);
    };
    let unread = messages::unread_count(&paths::messages_dir(&project_root), &scope, &agent);
    Ok(Some((state, unread)))
}

/// Apply `event` to the agent's marker. Returns the agent's state when the
/// marker changed.
pub(crate) fn apply(
    project_root: &Path,
    scope: &str,
    agent: &str,
    event: Option<WaitingEvent>,
) -> Result<Option<AgentState>> {
    let waiting = match event {
        None => return Ok(None),
        Some(WaitingEvent::Clear) => {
            let cleared = runtime::clear_waiting(project_root, scope, agent)?;
            return Ok(cleared.then_some(AgentState::Busy));
        }
        Some(WaitingEvent::ClearSubagent(subagent)) => {
            let held = runtime::read_waiting(project_root, scope, agent);
            if held.is_none_or(|h| h.subagent.as_deref() != Some(&subagent)) {
                return Ok(None);
            }
            runtime::clear_waiting(project_root, scope, agent)?;
            return Ok(Some(AgentState::Busy));
        }
        Some(WaitingEvent::Set(waiting)) => waiting,
        Some(WaitingEvent::Fill { waiting, over }) => {
            let held = runtime::read_waiting(project_root, scope, agent);
            if held.is_some_and(|h| !over.contains(&h.kind.class())) {
                return Ok(None);
            }
            waiting
        }
    };
    runtime::write_waiting(project_root, scope, agent, &waiting)?;
    Ok(Some(AgentState::from(waiting.kind.class())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::runtime::{Waiting, WaitingKind};
    use serde_json::json;
    use tempfile::tempdir;

    fn send(root: &Path, harness: Harness, payload: serde_json::Value) -> Option<AgentState> {
        apply(
            root,
            "login",
            "implementer",
            harness.waiting_event(&payload),
        )
        .unwrap()
    }

    fn kind(root: &Path) -> Option<WaitingKind> {
        runtime::read_waiting(root, "login", "implementer").map(|w| w.kind)
    }

    #[test]
    fn a_dialog_asks_until_its_tool_resolves() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let asked = send(
            root,
            Harness::ClaudeCode,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
                   "tool_input": {"questions": [{"question": "Which DB?"}]}}),
        );
        assert_eq!(asked, Some(AgentState::Asking));
        assert_eq!(
            runtime::read_waiting(root, "login", "implementer")
                .unwrap()
                .describe(),
            "Which DB?"
        );

        let resolved = send(
            root,
            Harness::ClaudeCode,
            json!({"hook_event_name": "PostToolUse", "tool_use_id": "t1"}),
        );
        assert_eq!(resolved, Some(AgentState::Busy));
        assert_eq!(kind(root), None);
    }

    #[test]
    fn a_subagents_dialog_is_resolved_only_by_that_subagents_tools() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let tool_done = |agent_id: Option<&str>| {
            let mut payload = json!({"hook_event_name": "PostToolUse", "tool_use_id": "t"});
            if let Some(id) = agent_id {
                payload["agent_id"] = json!(id);
            }
            send(root, Harness::ClaudeCode, payload)
        };
        let asked = send(
            root,
            Harness::ClaudeCode,
            json!({"hook_event_name": "PermissionRequest", "agent_id": "a1",
                   "agent_type": "general-purpose", "tool_name": "Bash",
                   "tool_input": {"command": "cargo test"}}),
        );
        assert_eq!(asked, Some(AgentState::Asking));

        assert_eq!(tool_done(Some("a2")), None, "another subagent's tool");
        assert_eq!(kind(root), Some(WaitingKind::Permission));
        assert_eq!(tool_done(Some("a1")), Some(AgentState::Busy));
        assert_eq!(kind(root), None);

        send(
            root,
            Harness::ClaudeCode,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash",
                   "tool_input": {"command": "ls"}}),
        );
        assert_eq!(tool_done(Some("a1")), None, "the main thread's dialog");
        assert_eq!(tool_done(None), Some(AgentState::Busy));
    }

    #[test]
    fn a_finished_tool_with_no_marker_changes_and_writes_nothing() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        for (harness, payload) in [
            (
                Harness::ClaudeCode,
                json!({"hook_event_name": "PostToolUse"}),
            ),
            (
                Harness::Codex,
                json!({"hook_event_name": "PreToolUse", "tool_name": "shell"}),
            ),
            (Harness::Codex, json!({"hook_event_name": "PostToolUse"})),
        ] {
            assert_eq!(send(root, harness, payload), None);
        }
        assert!(!root.join(".pm").exists(), "nothing written");
    }

    #[test]
    fn a_dialog_notification_never_hides_the_question_it_follows() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let notify = |root: &Path, kind: &str| {
            send(
                root,
                Harness::ClaudeCode,
                json!({"hook_event_name": "Notification", "notification_type": kind,
                       "message": "Claude needs your permission"}),
            )
        };
        send(
            root,
            Harness::ClaudeCode,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
                   "tool_input": {"questions": [{"question": "Which DB?"}]}}),
        );
        assert_eq!(notify(root, "permission_prompt"), None);
        assert_eq!(kind(root), Some(WaitingKind::Question));

        // At its prompt, a question it was asked is no longer on screen.
        assert_eq!(notify(root, "idle_prompt"), Some(AgentState::Unarmed));
        assert_eq!(kind(root), Some(WaitingKind::Prompt));

        runtime::write_waiting(
            root,
            "login",
            "implementer",
            &Waiting::now(WaitingKind::Interrupted, None),
        )
        .unwrap();
        assert_eq!(notify(root, "idle_prompt"), None, "keeps the cause");
        assert_eq!(kind(root), Some(WaitingKind::Interrupted));

        assert_eq!(
            notify(root, "permission_prompt"),
            Some(AgentState::Asking),
            "a dialog after the interrupt"
        );
        assert_eq!(kind(root), Some(WaitingKind::Dialog));

        runtime::write_waiting(
            root,
            "login",
            "implementer",
            &Waiting::now(WaitingKind::Background, None),
        )
        .unwrap();
        assert_eq!(notify(root, "permission_prompt"), Some(AgentState::Asking));
        assert_eq!(kind(root), Some(WaitingKind::Dialog));
    }

    #[test]
    fn a_codex_interrupt_unarms_and_the_opencode_plugin_resolves() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        assert_eq!(
            send(
                root,
                Harness::Codex,
                json!({"hook_event_name": "Interrupt"})
            ),
            Some(AgentState::Unarmed)
        );
        assert_eq!(
            send(
                root,
                Harness::OpenCode,
                json!({"hook_event_name": "Question", "detail": "Which DB?"})
            ),
            Some(AgentState::Asking)
        );
        assert_eq!(
            send(
                root,
                Harness::OpenCode,
                json!({"hook_event_name": "Resolved"})
            ),
            Some(AgentState::Busy)
        );
        assert_eq!(kind(root), None);
    }
}
