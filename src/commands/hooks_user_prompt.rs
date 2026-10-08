//! `pm harness hooks user-prompt`: the user typing into an agent's window
//! answers a blocked feature, so the feature goes back to `wip`.
//!
//! Only the user's input resets it. pm's own prompts are ignored: the
//! launch prompts a codex agent starts with ([`is_launch_prompt`]), a
//! continuation however it came — typed to re-arm the agent (see
//! `agent_rearm`), queued, or a rewake, which the harness wraps
//! ([`Harness::prompt_said`]) — and
//! the notice of a loop that stopped itself; so are prompts the harness
//! wrote itself ([`Harness::synthesized_prompt`]), a background task's end
//! among them. A `block` continuation runs no UserPromptSubmit. Blocked is
//! per feature, so input to any of its agents resets it.
//!
//! A continuation that reaches an agent with nothing unread is dropped: it
//! is a duplicate — a waiter's wake mid-turn, a typed re-arm, a queue item
//! from before a restart — whose messages an earlier prompt took. The hook
//! blocks it (`{"decision":"block"}`), so no turn runs, and does nothing
//! else: the waiter that marked the agent idle still waits.
//!
//! Any other prompt, pm's own included, means the agent is working again,
//! so it closes the dialogs its harness drops on a prompt
//! ([`hooks_dialog::close_typed`]), clears the agent's waiting marker
//! ([`runtime`]), claims a turn end its transcript recorded (an interrupt),
//! stamps its activity, and has its window published busy and its session
//! pushed. The interrupt is claimed
//! rather than left for the harness to bury under the prompt: the push may
//! read the transcript before the prompt reaches it.
//!
//! The harness adds the hook's stdout to the model's context, so it prints
//! nothing but a block, and always exits 0.

use std::io::Read;
use std::path::Path;

use serde_json::json;

use crate::commands::agent_spawn::is_launch_prompt;
use crate::commands::feat_status::feat_status;
use crate::commands::hooks_dialog;
use crate::commands::hooks_stop;
use crate::commands::running_agents;
use crate::error::Result;
use crate::harness::Harness;
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;
use crate::state::runtime;

/// What became of a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prompted {
    /// The agent works on it.
    Taken,
    /// A continuation with nothing unread, blocked.
    Dropped,
}

/// Run the hook. Always exit code 0, whatever happened. `on_prompt` gets
/// the agent's unread message count once it takes a prompt.
pub fn user_prompt(on_prompt: impl FnOnce(u32)) -> i32 {
    match user_prompt_inner() {
        Ok(Some((Prompted::Taken, unread))) => on_prompt(unread),
        Ok(Some((Prompted::Dropped, _))) => print!(
            "{}",
            json!({"decision": "block", "reason": "pm: dropped a wake for messages already read"})
        ),
        _ => {}
    }
    0
}

fn user_prompt_inner() -> Result<Option<(Prompted, u32)>> {
    let Some(agent) = std::env::var("PM_AGENT_NAME")
        .ok()
        .filter(|a| !a.is_empty())
    else {
        return Ok(None);
    };
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let Some(prompt) = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| v.get("prompt")?.as_str().map(str::to_string))
    else {
        return Ok(None);
    };
    let (project_root, scope) = paths::agent_scope()?;
    let prompted = on_prompt(&project_root, &scope, &agent, &prompt)?;
    let unread = messages::unread_count(&paths::messages_dir(&project_root), &scope, &agent);
    Ok(Some((prompted, unread)))
}

/// Any prompt to `agent`: drop a continuation with nothing unread, else
/// unblock its feature if the prompt is the user's, then clear its marker
/// and claim its transcript's turn end.
fn on_prompt(project_root: &Path, scope: &str, agent: &str, prompt: &str) -> Result<Prompted> {
    let registry = AgentRegistry::load(&paths::agents_dir(project_root), scope)?;
    let harness = registry.get(agent).map(|e| e.harness);
    let said = harness.map_or(prompt, |h| h.prompt_said(prompt));
    let unread = messages::unread_count(&paths::messages_dir(project_root), scope, agent);
    if hooks_stop::is_continuation(said) && unread == 0 {
        return Ok(Prompted::Dropped);
    }
    on_user_prompt(project_root, scope, prompt, harness)?;
    if let Some(harness) = harness {
        hooks_dialog::close_typed(project_root, scope, agent, harness, prompt)?;
    }
    runtime::touch_activity(project_root, scope, agent)?;
    runtime::clear_waiting(project_root, scope, agent)?;
    if let Some(harness) = harness
        && let Some(ended) = running_agents::waiting(project_root, scope, agent, harness)
        && let Some(id) = ended.entry
    {
        runtime::claim_turn_end(project_root, scope, agent, &id)?;
    }
    Ok(Prompted::Taken)
}

/// Whether `prompt` to an agent running `harness` is the user's: neither
/// one pm sent — a launch prompt, a continuation however it came, a stopped
/// loop's notice — nor one the harness wrote.
fn is_users(prompt: &str, harness: Option<Harness>) -> bool {
    let said = harness.map_or(prompt, |h| h.prompt_said(prompt));
    !is_launch_prompt(said)
        && !hooks_stop::is_continuation(said)
        && !hooks_stop::is_loop_notice(said)
        && !harness.is_some_and(|h| h.synthesized_prompt(prompt))
}

/// Set `scope` back to `wip` if it is a blocked feature and `prompt`, to an
/// agent running `harness`, is the user's. Returns whether it did.
fn on_user_prompt(
    project_root: &Path,
    scope: &str,
    prompt: &str,
    harness: Option<Harness>,
) -> Result<bool> {
    if scope == "main" || !is_users(prompt, harness) {
        return Ok(false);
    }
    let state = FeatureState::load(&paths::features_dir(project_root), scope)?;
    if state.progress != Progress::Blocked {
        return Ok(false);
    }
    feat_status(project_root, scope, Progress::Wip, None, None)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_spawn::{RESUME_PROMPT, SPAWN_PROMPT};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn state(project: &Path) -> FeatureState {
        FeatureState::load(&paths::features_dir(project), "login").unwrap()
    }

    fn blocked_feature(dir: &Path) -> std::path::PathBuf {
        let (project, _) = TestServer::new().setup_project_with_feature_no_tmux(dir, "login");
        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            Some("implementer"),
        )
        .unwrap();
        project
    }

    #[test]
    fn the_users_prompt_unblocks_the_feature_and_drops_the_reason_and_agent() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());

        assert!(on_user_prompt(&project, "login", "use postgres", None).unwrap());

        let state = state(&project);
        assert_eq!(state.progress, Progress::Wip);
        assert_eq!(state.blocked_reason, None);
        assert_eq!(state.blocked_by, None);
    }

    #[test]
    fn the_launch_prompts_leave_the_feature_blocked() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());

        assert!(!on_user_prompt(&project, "login", SPAWN_PROMPT, None).unwrap());
        assert!(!on_user_prompt(&project, "login", RESUME_PROMPT, None).unwrap());

        let state = state(&project);
        assert_eq!(state.progress, Progress::Blocked);
        assert_eq!(state.blocked_reason.as_deref(), Some("which DB?"));
    }

    #[test]
    fn a_re_arm_prompt_leaves_the_feature_blocked() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());
        crate::messages::send(
            &paths::messages_dir(&project),
            "login",
            "implementer",
            "reviewer",
            "hi",
        )
        .unwrap();
        let rearm = hooks_stop::continuation(&project, "login", "implementer").unwrap();

        assert!(!on_user_prompt(&project, "login", &rearm, None).unwrap());
        assert_eq!(state(&project).progress, Progress::Blocked);
        // The user quoting it in a longer prompt is still the user.
        let quoted = format!("why did you get \"{rearm}\"?");
        assert!(on_user_prompt(&project, "login", &quoted, None).unwrap());
    }

    #[test]
    fn a_feature_that_is_not_blocked_is_left_alone() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        std::fs::write(
            crate::commands::feat_summary::path(&project, "login").unwrap(),
            "notes",
        )
        .unwrap();
        feat_status(&project, "login", Progress::Ready, None, None).unwrap();

        assert!(!on_user_prompt(&project, "login", "one more thing", None).unwrap());
        assert_eq!(state(&project).progress, Progress::Ready);
    }

    #[test]
    fn main_scope_does_nothing() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());

        assert!(!on_user_prompt(&project, "main", "use postgres", None).unwrap());
        assert_eq!(state(&project).progress, Progress::Blocked);
    }

    #[test]
    fn any_prompt_clears_the_agents_marker_in_main_too() {
        use crate::state::runtime::{Waiting, WaitingKind};
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());
        let interrupted = Waiting::now(WaitingKind::Interrupted, None);

        runtime::write_waiting(&project, "main", "main", &interrupted).unwrap();
        on_prompt(&project, "main", "main", "go on").unwrap();
        assert_eq!(runtime::read_waiting(&project, "main", "main"), None);

        runtime::write_waiting(&project, "login", "implementer", &interrupted).unwrap();
        on_prompt(&project, "login", "implementer", SPAWN_PROMPT).unwrap();
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer"),
            None
        );
        assert_eq!(state(&project).progress, Progress::Blocked);
    }

    #[test]
    fn a_prompt_ends_an_interrupt_the_transcript_still_shows() {
        use crate::commands::running_agents::waiting;
        use crate::state::agent::{AgentEntry, AgentType};
        use crate::state::runtime::{SessionPath, WaitingKind};
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());
        let mut registry = AgentRegistry::default();
        registry.register(
            "implementer",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "implementer".into(),
                active: true,
                agent_definition: None,
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry
            .save(&paths::agents_dir(&project), "login")
            .unwrap();
        let transcript = dir.path().join("session.jsonl");
        let interrupt =
            r#"{"type":"user","uuid":"u1","message":{"content":"[Request interrupted by user]"}}"#;
        std::fs::write(&transcript, format!("{interrupt}\n")).unwrap();
        runtime::write_session_path(
            &project,
            "login",
            "implementer",
            SessionPath::Transcript,
            Some(&transcript),
        )
        .unwrap();
        let at = || waiting(&project, "login", "implementer", Harness::ClaudeCode).map(|w| w.kind);
        assert_eq!(at(), Some(WaitingKind::Interrupted));

        on_prompt(&project, "login", "implementer", "keep going").unwrap();
        assert_eq!(at(), None, "busy before the prompt reaches the transcript");
    }

    fn register(project: &Path, harness: Harness) {
        use crate::state::agent::{AgentEntry, AgentType};
        let mut registry = AgentRegistry::default();
        registry.register(
            "implementer",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "implementer".into(),
                active: true,
                agent_definition: None,
                harness,
                spawned_at: None,
            },
        );
        registry.save(&paths::agents_dir(project), "login").unwrap();
    }

    /// Claude Code's prompts for a rewake carrying `reason`, naming the
    /// hook by its event, by pm's own command, whose quotes the reason must
    /// be found past (2.1.289), and as SessionStart's waiter (2.1.294).
    fn rewakes(reason: &str) -> Vec<String> {
        let command = crate::commands::hooks_install::stop_hook_command(Harness::ClaudeCode);
        [
            "\"Stop\"".to_string(),
            format!(": \"{command}\""),
            "\"SessionStart:startup\"".to_string(),
            "\"SessionStart:resume\"".to_string(),
        ]
        .iter()
        .map(|named| {
            format!(
                "<task-notification>\n<summary>Stop hook feedback</summary>\n<system-reminder>\n\
                 Stop hook blocking error from command{}{named}: {reason}\n</system-reminder>\n\
                 </task-notification>",
                if named.starts_with(':') { "" } else { " " }
            )
        })
        .collect()
    }

    #[test]
    fn a_continuation_with_nothing_unread_is_dropped_and_leaves_the_agent_idle() {
        use crate::state::runtime::{Waiting, WaitingKind};
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());
        register(&project, Harness::ClaudeCode);
        let messages = paths::messages_dir(&project);
        crate::messages::send(&messages, "login", "implementer", "reviewer", "hi").unwrap();
        let continuation = hooks_stop::continuation(&project, "login", "implementer").unwrap();
        crate::messages::next(&messages, "login", "implementer", "reviewer").unwrap();
        let idle = Waiting::now(WaitingKind::Idle, None);
        runtime::write_waiting(&project, "login", "implementer", &idle).unwrap();

        for prompt in std::iter::once(continuation.clone()).chain(rewakes(&continuation)) {
            assert_eq!(
                on_prompt(&project, "login", "implementer", &prompt).unwrap(),
                Prompted::Dropped
            );
            assert_eq!(
                runtime::read_waiting(&project, "login", "implementer"),
                Some(idle.clone())
            );
        }
        assert_eq!(state(&project).progress, Progress::Blocked);
    }

    #[test]
    fn a_background_tasks_end_is_not_the_user() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());
        register(&project, Harness::ClaudeCode);
        let notification = "<task-notification>\n<task-id>bdfpbsu9n</task-id>\n\
            <status>completed</status>\n<summary>Background command \"Run tests\" \
            completed (exit code 0)</summary>\n</task-notification>";

        assert_eq!(
            on_prompt(&project, "login", "implementer", notification).unwrap(),
            Prompted::Taken
        );
        assert_eq!(state(&project).progress, Progress::Blocked);
    }

    #[test]
    fn a_wake_with_messages_unread_is_taken_and_leaves_the_feature_blocked() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());
        register(&project, Harness::ClaudeCode);
        let messages = paths::messages_dir(&project);
        crate::messages::send(&messages, "login", "implementer", "reviewer", "hi").unwrap();
        let continuation = hooks_stop::continuation(&project, "login", "implementer").unwrap();

        for prompt in std::iter::once(continuation.clone()).chain(rewakes(&continuation)) {
            assert_eq!(
                on_prompt(&project, "login", "implementer", &prompt).unwrap(),
                Prompted::Taken
            );
        }
        let notice = "pm: never-idle loop stopped: 5 wakes read nothing. This agent no longer \
                      wakes for messages; restart it with `pm agent restart implementer`.";
        for prompt in rewakes(notice) {
            assert_eq!(
                on_prompt(&project, "login", "implementer", &prompt).unwrap(),
                Prompted::Taken
            );
        }
        assert_eq!(state(&project).progress, Progress::Blocked);

        // On a harness that wraps no wake, the wrapping is the user's text.
        register(&project, Harness::Codex);
        on_prompt(&project, "login", "implementer", &rewakes(&continuation)[1]).unwrap();
        assert_eq!(state(&project).progress, Progress::Wip);
    }

    #[test]
    fn a_prompt_closes_a_codex_question_answering_it_only_when_it_is_the_reply() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());
        register(&project, Harness::Codex);
        let ask = || {
            let payload = json!({"hook_event_name": "PreToolUse",
                "tool_name": "request_user_input_async", "tool_use_id": "call_9Xk2",
                "tool_input": {"questions": [{"title": "Which colour?"}]}});
            let (dialog, reply_context) = Harness::Codex.typed_dialog(&payload).unwrap();
            runtime::write_waiting(&project, "login", "implementer", &dialog.waiting()).unwrap();
            let record = runtime::DialogRecord {
                dialog,
                pid: None,
                reply_context,
            };
            runtime::write_dialog(&project, "login", "implementer", &record).unwrap();
            record
        };
        // As codex's panel submitted it, live.
        let reply = "<send_user_message_question_reply>\n[{\"answer\":\"Blue\",\
            \"question\":\"Which colour?\",\
            \"questionItemId\":\"[\\\"request_user_input_async\\\",\\\"call_9Xk2\\\",0]\"}]\n\
            </send_user_message_question_reply>";

        let answered = ask();
        assert_eq!(
            on_prompt(&project, "login", "implementer", reply).unwrap(),
            Prompted::Taken
        );
        assert!(runtime::dialog_answer_taken(
            &project,
            "login",
            "implementer",
            &answered.dialog.id
        ));
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer"),
            None
        );
        assert_eq!(state(&project).progress, Progress::Wip, "the user's answer");

        let dropped = ask();
        on_prompt(&project, "login", "implementer", "skip that").unwrap();
        assert!(runtime::dialog_closed(
            &project,
            "login",
            "implementer",
            &dropped.dialog.id
        ));
        assert!(!runtime::dialog_answer_taken(
            &project,
            "login",
            "implementer",
            &dropped.dialog.id
        ));
    }
}
