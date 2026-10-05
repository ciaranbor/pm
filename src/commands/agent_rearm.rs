//! Re-arms an agent no message can wake (unarmed, at its prompt) by typing
//! the prompt the Stop hook would have returned, so it reads its messages
//! and its next turn ends in the hook again.
//!
//! Keystrokes reach whatever the window shows, so every condition guards
//! against typing into something other than an empty prompt: the marker
//! must say unarmed (never asking — the keys would answer the dialog — nor
//! a loop that stopped itself on purpose), the window must run its harness
//! and not the hook, and the harness must read its input line as empty,
//! once any key it names to make the line take text has been pressed (vim
//! NORMAL mode). A draft is never cleared; when any check fails the message
//! just stays queued and the agent stays visibly unarmed. Removing the
//! marker is the claim to type, so of two concurrent senders only one does;
//! the prompt's UserPromptSubmit would clear it anyway. A turn's end read
//! from the transcript is claimed by its entry instead
//! ([`runtime::claim_turn_end`]). The claim comes before any key is
//! pressed, and is given back when nothing is typed.

use std::path::Path;

use crate::error::Result;
use crate::harness::Harness;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig, resolve_harness_config};
use crate::state::runtime::{self, Waiting, WaitingClass, WaitingKind};
use crate::tmux;

use super::agent_input::input_line_ready;
use super::hooks_stop;
use super::running_agents::{Liveness, Windows, liveness, waiting};

/// Re-arm `agent` if it is unarmed at an empty prompt. Returns the marker
/// it was re-armed from, or `None` when it was left alone.
pub fn rearm(
    project_root: &Path,
    scope: &str,
    agent: &str,
    tmux_server: Option<&str>,
) -> Result<Option<Waiting>> {
    let registry = AgentRegistry::load(&paths::agents_dir(project_root), scope)?;
    let Some(entry) = registry.get(agent).filter(|e| e.active) else {
        return Ok(None);
    };
    let harness = entry.harness;
    let Some(waiting) = waiting(project_root, scope, agent, harness)
        .filter(|w| w.kind.class() == WaitingClass::Unarmed && w.kind != WaitingKind::Tripped)
    else {
        return Ok(None);
    };
    if harness.loop_stopped(project_root, scope, agent).is_some() {
        return Ok(None);
    }
    let project = ProjectConfig::load(&paths::pm_dir(project_root))?;
    let config = resolve_harness_config(&project.harness, &GlobalConfig::load_or_default().harness);
    let session = tmux::session_name(&project.project.name, scope);
    let windows = Windows::read(tmux_server)?;
    let Some(pane) = windows.find(&session, &entry.window_name) else {
        return Ok(None);
    };
    if liveness(windows.processes(pane).as_deref(), harness, &config) != Liveness::Busy {
        return Ok(None);
    }
    let from_marker = runtime::read_waiting(project_root, scope, agent).as_ref() == Some(&waiting);
    let entry = waiting.entry.as_deref().unwrap_or_default();
    let claimed = if from_marker {
        runtime::clear_waiting(project_root, scope, agent)?
    } else {
        !entry.is_empty() && runtime::claim_turn_end(project_root, scope, agent, entry)?
    };
    if !claimed {
        return Ok(None);
    }
    let release = || {
        if from_marker {
            runtime::write_waiting(project_root, scope, agent, &waiting)
        } else {
            runtime::release_turn_end(project_root, scope, agent, entry)
        }
    };
    match type_prompt(project_root, scope, agent, harness, &pane.id, tmux_server) {
        Ok(true) => Ok(Some(waiting)),
        Ok(false) => release().map(|()| None),
        Err(e) => {
            let _ = release();
            Err(e)
        }
    }
}

/// Type the agent's messages prompt into `pane` if its input line is empty.
/// Returns whether it typed.
fn type_prompt(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    pane: &str,
    tmux_server: Option<&str>,
) -> Result<bool> {
    if !input_line_ready(project_root, scope, agent, harness, pane, tmux_server)? {
        return Ok(false);
    }
    let prompt = hooks_stop::continuation(project_root, scope, agent)?;
    tmux::send_text(tmux_server, pane, &prompt)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_send::agent_send;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    const PROMPT: &str = "You have new messages from reviewer. Run `pm msg read` to read them.";

    /// A feature whose `implementer` sits at an empty Claude Code prompt
    /// with `marker` as its waiting marker.
    fn at_prompt(
        server: &TestServer,
        dir: &Path,
        marker: WaitingKind,
    ) -> (std::path::PathBuf, String) {
        let (project, project_name) = server.setup_project_with_feature(dir, "login");
        let session = tmux::session_name(&project_name, "login");
        let target = server.spawn_prompting_fake_agent(&project, &session, "login", "implementer");
        server.wait_for_pane_text(&target, "❯");
        let waiting = Waiting::now(marker, None);
        runtime::write_waiting(&project, "login", "implementer", &waiting).unwrap();
        (project, target)
    }

    fn send(server: &TestServer, project: &Path) -> String {
        agent_send(
            project,
            "login",
            None,
            "implementer",
            "reviewer",
            "hi",
            server.name(),
        )
        .unwrap()
        .status
    }

    fn screen(server: &TestServer, target: &str) -> String {
        tmux::capture_pane(server.name(), target).unwrap()
    }

    #[test]
    fn an_unarmed_agent_at_an_empty_prompt_is_typed_its_messages_prompt() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::HookEnded);
        let users = server.split_before(&target);

        let status = send(&server, &project);

        assert!(
            status.ends_with("\nRe-armed 'implementer' (Stop hook ended)"),
            "{status}"
        );
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer"),
            None
        );
        server.wait_for_pane_text(&target, &format!("❯ {PROMPT}"));
        let users = server.tmux_stdout(&["capture-pane", "-p", "-t", &users]);
        assert!(!users.contains("You have new messages"), "{users}");
    }

    #[test]
    fn an_agent_asking_the_user_is_left_alone() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::Permission);

        let status = send(&server, &project);

        assert_eq!(
            status,
            "Message 001 sent to 'implementer' (from 'reviewer')"
        );
        assert!(!screen(&server, &target).contains("You have new messages"));
    }

    #[test]
    fn a_loop_that_stopped_itself_is_left_alone() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::Tripped);

        let status = send(&server, &project);

        assert_eq!(
            status,
            "Message 001 sent to 'implementer' (from 'reviewer')"
        );
        assert!(!screen(&server, &target).contains("You have new messages"));
    }

    #[test]
    fn a_draft_in_the_input_line_is_left_untouched() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::Prompt);
        let mut tmux = std::process::Command::new("tmux");
        if let Some(name) = server.name() {
            tmux.args(["-L", name]);
        }
        let typed = tmux
            .args(["send-keys", "-t", &target, "-l", "half a thought"])
            .status()
            .unwrap();
        assert!(typed.success());
        server.wait_for_pane_text(&target, "❯ half a thought");

        let status = send(&server, &project);

        assert_eq!(
            status,
            "Message 001 sent to 'implementer' (from 'reviewer')"
        );
        let screen = screen(&server, &target);
        assert!(screen.contains("❯ half a thought"), "{screen}");
        assert!(!screen.contains("You have new messages"), "{screen}");
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer").map(|w| w.kind),
            Some(WaitingKind::Prompt)
        );
    }

    #[test]
    fn an_agent_interrupted_without_a_hook_is_re_armed_from_its_transcript() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::Permission);
        let transcript = dir.path().join("session.jsonl");
        let interrupt = serde_json::json!({
            "type": "user",
            "uuid": "5b0c7e1a-interrupt",
            "message": {"content": [{"type": "text", "text": "[Request interrupted by user for tool use]"}]},
        });
        std::fs::write(&transcript, format!("{interrupt}\n")).unwrap();
        std::fs::File::options()
            .append(true)
            .open(&transcript)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(1))
            .unwrap();
        runtime::write_session_path(
            &project,
            "login",
            "implementer",
            runtime::SessionPath::Transcript,
            Some(&transcript),
        )
        .unwrap();

        let status = send(&server, &project);

        assert!(
            status.ends_with("\nRe-armed 'implementer' (interrupted)"),
            "{status}"
        );
        assert_eq!(
            waiting(&project, "login", "implementer", Harness::ClaudeCode),
            None,
            "a re-armed interrupt no longer counts"
        );
        server.wait_for_pane_text(&target, &format!("❯ {PROMPT}"));

        let bookkeeping = serde_json::json!({"type": "system", "uuid": "5b0c7e1a-later"});
        let mut file = std::fs::File::options()
            .append(true)
            .open(&transcript)
            .unwrap();
        std::io::Write::write_all(&mut file, format!("{bookkeeping}\n").as_bytes()).unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            waiting(&project, "login", "implementer", Harness::ClaudeCode),
            None,
            "nor once the transcript has moved on past it"
        );
    }

    #[test]
    fn a_vim_key_the_prompt_takes_as_text_is_erased_and_the_agent_left_alone() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::Prompt);
        let config_dir = dir.path().join("claude-config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join(".claude.json"), r#"{"editorMode":"vim"}"#).unwrap();
        runtime::write_session_path(
            &project,
            "login",
            "implementer",
            runtime::SessionPath::ConfigDir,
            Some(&config_dir),
        )
        .unwrap();

        let status = send(&server, &project);

        assert_eq!(
            status,
            "Message 001 sent to 'implementer' (from 'reviewer')"
        );
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer").map(|w| w.kind),
            Some(WaitingKind::Prompt)
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while screen(&server, &target).contains("❯ i") {
            assert!(
                std::time::Instant::now() < deadline,
                "{}",
                screen(&server, &target)
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!screen(&server, &target).contains("You have new messages"));
    }
}
