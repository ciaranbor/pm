//! `pm harness hooks user-prompt`: the user typing into an agent's window
//! answers a blocked feature, so the feature goes back to `wip`.
//!
//! Only the user's input resets it. pm messages arrive as Stop-hook
//! continuations, which no harness runs this hook for, or as the same text
//! typed in to re-arm an agent (see `agent_rearm`); that, pm's own launch
//! prompts ([`is_launch_prompt`]) and prompts the harness wrote itself
//! ([`Harness::synthesized_prompt`]) are ignored. Blocked is per feature,
//! so input to any of its agents resets it.
//!
//! Any prompt, pm's own included, also means the agent is working again, so
//! it clears the agent's waiting marker ([`runtime`]), claims a turn end its
//! transcript recorded (an interrupt), stamps its activity, and has its
//! window published busy and its session pushed. The interrupt is claimed
//! rather than left for the harness to bury under the prompt: the push may
//! read the transcript before the prompt reaches it.
//!
//! The user's prompt to a harness that runs this hook as it queues text
//! behind pm's Stop hook
//! ([`Harness::prompt_hook_runs_when_held`](crate::harness::Harness::prompt_hook_runs_when_held))
//! files a yield request as `pm serve`'s typed text does, gated as in
//! [`agent_input`].
//!
//! The harness adds the hook's stdout to the model's context and may refuse
//! the prompt on a non-zero exit, so it prints nothing and always exits 0.

use std::io::Read;
use std::path::Path;

use crate::commands::agent_input;
use crate::commands::agent_spawn::is_launch_prompt;
use crate::commands::feat_status::feat_status;
use crate::commands::hooks_stop;
use crate::commands::running_agents::{self, Liveness, Windows};
use crate::error::Result;
use crate::harness::Harness;
use crate::messages;
use crate::state::agent::{AgentEntry, AgentRegistry};
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig, resolve_harness_config};
use crate::state::runtime;
use crate::tmux;

/// Run the hook. Always exit code 0, whatever happened. `on_prompt` gets
/// the agent's unread message count.
pub fn user_prompt(tmux_server: Option<&str>, on_prompt: impl FnOnce(u32)) -> i32 {
    if let Ok(Some(unread)) = user_prompt_inner(tmux_server) {
        on_prompt(unread);
    }
    0
}

fn user_prompt_inner(tmux_server: Option<&str>) -> Result<Option<u32>> {
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
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let scope = paths::resolve_scope_from(&project_root, &cwd)?;
    on_prompt(&project_root, &scope, &agent, &prompt, tmux_server)?;
    let unread = messages::unread_count(&paths::messages_dir(&project_root), &scope, &agent);
    Ok(Some(unread))
}

/// Any prompt to `agent`: have its Stop hook yield for the user's prompt
/// should its harness hold it, unblock its feature if the prompt is the
/// user's, then clear its marker and claim its transcript's turn end.
fn on_prompt(
    project_root: &Path,
    scope: &str,
    agent: &str,
    prompt: &str,
    tmux_server: Option<&str>,
) -> Result<()> {
    let registry = AgentRegistry::load(&paths::agents_dir(project_root), scope)?;
    let entry = registry.get(agent);
    let harness = entry.map(|e| e.harness);
    if let Some(entry) = entry
        && entry.harness.prompt_hook_runs_when_held()
        && is_users(prompt, harness)
    {
        // Best-effort: the prompt's other effects must not wait on it. It
        // reads the waiting marker, so it goes before the marker is cleared.
        let _ = yield_if_held(project_root, scope, agent, entry, prompt, tmux_server);
    }
    on_user_prompt(project_root, scope, prompt, harness)?;
    runtime::touch_activity(project_root, scope, agent)?;
    runtime::clear_waiting(project_root, scope, agent)?;
    if let Some(harness) = harness
        && let Some(ended) = running_agents::waiting(project_root, scope, agent, harness)
        && let Some(id) = ended.entry
    {
        runtime::claim_turn_end(project_root, scope, agent, &id)?;
    }
    Ok(())
}

/// File a yield request for `prompt` if `agent`'s next turn end runs pm's
/// Stop hook.
fn yield_if_held(
    project_root: &Path,
    scope: &str,
    agent: &str,
    entry: &AgentEntry,
    prompt: &str,
    tmux_server: Option<&str>,
) -> Result<()> {
    let project = ProjectConfig::load(&paths::pm_dir(project_root))?;
    let session = tmux::session_name(&project.project.name, scope);
    let windows = Windows::read(tmux_server)?;
    let Some(pane) = windows.find(&session, &entry.window_name) else {
        return Ok(());
    };
    let config = resolve_harness_config(&project.harness, &GlobalConfig::load_or_default().harness);
    let processes = windows.processes(pane);
    let liveness = running_agents::liveness(processes.as_deref(), entry.harness, &config);
    let at = match liveness {
        Liveness::Busy => running_agents::waiting(project_root, scope, agent, entry.harness),
        _ => None,
    };
    if !agent_input::hook_runs_next(liveness, at.as_ref()) {
        return Ok(());
    }
    let after = agent_input::conversation_end(project_root, scope, agent)?;
    let typed = entry.harness.typed_prompt(prompt);
    agent_input::request_yield(project_root, scope, agent, &typed, after)
}

/// Whether `prompt` to an agent running `harness` is the user's: neither
/// one pm typed (a launch prompt or a re-arm) nor one the harness wrote.
fn is_users(prompt: &str, harness: Option<Harness>) -> bool {
    !is_launch_prompt(prompt)
        && !hooks_stop::is_continuation(prompt)
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
    use crate::state::runtime::YieldRequest;
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
        on_prompt(&project, "main", "main", "go on", None).unwrap();
        assert_eq!(runtime::read_waiting(&project, "main", "main"), None);

        runtime::write_waiting(&project, "login", "implementer", &interrupted).unwrap();
        on_prompt(&project, "login", "implementer", SPAWN_PROMPT, None).unwrap();
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

        on_prompt(&project, "login", "implementer", "keep going", None).unwrap();
        assert_eq!(at(), None, "busy before the prompt reaches the transcript");
    }

    /// A feature whose `implementer` runs `harness`, its window reading as
    /// `liveness`: idle runs a process carrying pm's Stop hook.
    fn agent(
        server: &TestServer,
        dir: &Path,
        harness: crate::harness::Harness,
        liveness: running_agents::Liveness,
    ) -> std::path::PathBuf {
        let (project, name) = server.setup_project_with_feature(dir, "login");
        let session = tmux::session_name(&name, "login");
        let shell = crate::testing::fake_harness_binary(harness, Path::new("/bin/bash"));
        let job = match liveness {
            running_agents::Liveness::Idle => format!(
                "sh -c 'sleep 999; :' {}",
                crate::commands::hooks_install::PM_HOOK_MARKER
            ),
            _ => "sleep 999".into(),
        };
        let command = format!("{} -c \"{job}; :\"", shell.display());
        server.spawn_harness_agent(
            &project,
            &session,
            "login",
            "implementer",
            harness,
            &command,
            liveness,
        );
        project
    }

    fn yield_request(project: &Path) -> Option<YieldRequest> {
        runtime::take_yield_request(project, "login", "implementer").unwrap()
    }

    #[test]
    fn the_users_prompt_to_a_claude_code_agent_in_its_stop_hook_asks_it_to_yield() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let project = agent(
            &server,
            dir.path(),
            Harness::ClaudeCode,
            running_agents::Liveness::Idle,
        );
        let prompt =
            |text: &str| on_prompt(&project, "login", "implementer", text, server.name()).unwrap();

        prompt("use postgres");
        let request = yield_request(&project).expect("a yield request");
        assert_eq!(request.text_sha256, agent_input::sha256("use postgres"));

        // A long paste is matched as typed, as the conversation shows it.
        prompt("see:\n<pasted_content id=\"e801\">\nline 1\nline 2\n</pasted_content id=\"e801\">");
        let request = yield_request(&project).expect("a yield request");
        assert_eq!(
            request.text_sha256,
            agent_input::sha256("see:\nline 1\nline 2")
        );

        prompt(SPAWN_PROMPT);
        assert_eq!(yield_request(&project), None);
        crate::messages::send(
            &paths::messages_dir(&project),
            "login",
            "implementer",
            "reviewer",
            "hi",
        )
        .unwrap();
        prompt(&hooks_stop::continuation(&project, "login", "implementer").unwrap());
        assert_eq!(yield_request(&project), None);
    }

    #[test]
    fn a_claude_code_agent_mid_turn_is_asked_to_yield_but_not_one_at_its_prompt() {
        use crate::state::runtime::{Waiting, WaitingKind};
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let project = agent(
            &server,
            dir.path(),
            Harness::ClaudeCode,
            running_agents::Liveness::Busy,
        );

        on_prompt(&project, "login", "implementer", "also this", server.name()).unwrap();
        assert!(yield_request(&project).is_some());

        let prompt = Waiting::now(WaitingKind::Prompt, None);
        runtime::write_waiting(&project, "login", "implementer", &prompt).unwrap();
        on_prompt(&project, "login", "implementer", "go on", server.name()).unwrap();
        assert_eq!(yield_request(&project), None);
    }

    #[test]
    fn a_background_tasks_end_mid_turn_is_not_the_user() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let project = agent(
            &server,
            dir.path(),
            Harness::ClaudeCode,
            running_agents::Liveness::Busy,
        );
        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            None,
        )
        .unwrap();
        let notification = "<task-notification>\n<task-id>bdfpbsu9n</task-id>\n\
            <status>completed</status>\n<summary>Background command \"Run tests\" \
            completed (exit code 0)</summary>\n</task-notification>";

        on_prompt(
            &project,
            "login",
            "implementer",
            notification,
            server.name(),
        )
        .unwrap();

        assert_eq!(yield_request(&project), None);
        assert_eq!(state(&project).progress, Progress::Blocked);
    }

    #[test]
    fn a_harness_that_runs_no_hook_as_it_queues_text_is_never_asked_to_yield() {
        for harness in [Harness::OpenCode, Harness::Codex] {
            let server = TestServer::new();
            let dir = tempdir().unwrap();
            let project = agent(&server, dir.path(), harness, running_agents::Liveness::Idle);

            on_prompt(
                &project,
                "login",
                "implementer",
                "use postgres",
                server.name(),
            )
            .unwrap();
            assert_eq!(yield_request(&project), None, "{harness:?}");
        }
    }
}
