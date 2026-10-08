//! Re-arms an agent no message can wake (unarmed, at its prompt) by typing
//! the prompt its waiter would have given, so it reads its messages and its
//! next turn's end starts a waiter again.
//!
//! Keystrokes reach whatever the window shows, so every condition guards
//! against typing into something other than an empty prompt: the marker
//! must say unarmed (never asking — the keys would answer the dialog — nor
//! a loop that stopped itself on purpose), the window must run its harness
//! with no live waiter, the pane must not be in use ([`tmux::panes::in_use`]:
//! in a mode, or in front of an attached client — never cancelled, since
//! either is the user's), and the harness must read its input line as empty
//! and taking text. No key is pressed to change the harness's own mode: a
//! box in vim NORMAL mode, like a dialog, reads as unknown and is left
//! alone. A draft is never cleared; when any check fails the message
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
use crate::state::runtime::{self, SessionPath, Waiting, WaitingClass, WaitingKind};
use crate::tmux;

use super::hooks_stop;
use super::running_agents::{AgentAt, Liveness, Windows, classify};

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
    let agent_at = AgentAt {
        project_root,
        scope,
        name: agent,
        harness,
    };
    let Some(waiting) = (match classify(agent_at, windows.processes(pane).as_deref(), &config) {
        (Liveness::Busy, at) => at,
        _ => None,
    })
    .filter(|w| w.kind.class() == WaitingClass::Unarmed && w.kind != WaitingKind::Tripped) else {
        return Ok(None);
    };
    let marker = runtime::read_waiting(project_root, scope, agent)
        .filter(|m| m.since == waiting.since && waiting.entry.is_none());
    let entry = waiting.entry.as_deref().unwrap_or_default();
    let claimed = if marker.is_some() {
        runtime::clear_waiting(project_root, scope, agent)?
    } else {
        !entry.is_empty() && runtime::claim_turn_end(project_root, scope, agent, entry)?
    };
    if !claimed {
        return Ok(None);
    }
    let release = || {
        if let Some(marker) = &marker {
            runtime::write_waiting(project_root, scope, agent, marker)
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

/// Type the agent's messages prompt into `pane` if it is not in use and its
/// input line is empty. Returns whether it typed.
fn type_prompt(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    pane: &str,
    tmux_server: Option<&str>,
) -> Result<bool> {
    if tmux::panes::in_use(tmux_server, pane)? {
        return Ok(false);
    }
    let home = paths::home_dir()?;
    let config_dir = runtime::read_session_path(project_root, scope, agent, SessionPath::ConfigDir);
    let screen = tmux::capture_screen(tmux_server, pane)?;
    if harness.input_is_empty(&screen, &home, config_dir.as_deref()) != Some(true) {
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
    use crate::commands::running_agents::waiting;
    use crate::testing::{ControlClient, TestServer};
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
        let server = TestServer::own("rearm-unarmed");
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::HookEnded);
        let users = server.split_before(&target);
        let session = server.tmux_stdout(&["display-message", "-p", "-t", &target, "#S"]);
        let _viewing_the_users_pane = ControlClient::attach(server.name(), &session);

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

    /// Send the agent a message and check it is left alone, still unarmed.
    fn assert_left_alone(server: &TestServer, project: &Path) {
        let status = send(server, project);

        assert_eq!(
            status,
            "Message 001 sent to 'implementer' (from 'reviewer')"
        );
        assert_eq!(
            runtime::read_waiting(project, "login", "implementer").map(|w| w.kind),
            Some(WaitingKind::HookEnded)
        );
    }

    fn a_pane_in_a_mode_is_left_in_it_untyped(enter: &str) {
        let server = TestServer::own(&format!("rearm-{enter}"));
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::HookEnded);
        let pane = server.pane_id(&target);
        server.tmux_stdout(&[enter, "-t", &pane]);
        server.split_before(&target);
        let session = server.tmux_stdout(&["display-message", "-p", "-t", &target, "#S"]);
        let _viewing_another_pane = ControlClient::attach(server.name(), &session);

        assert_left_alone(&server, &project);

        assert!(tmux::paste::in_mode(server.name(), &pane).unwrap());
    }

    #[test]
    fn a_pane_in_copy_mode_is_left_in_it_untyped() {
        a_pane_in_a_mode_is_left_in_it_untyped("copy-mode");
    }

    #[test]
    fn a_pane_in_tree_mode_is_left_in_it_untyped() {
        a_pane_in_a_mode_is_left_in_it_untyped("choose-tree");
    }

    #[test]
    fn a_pane_a_client_is_viewing_is_left_untyped() {
        let server = TestServer::own("rearm-viewed");
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::HookEnded);
        server.tmux_stdout(&["select-window", "-t", &target]);
        let session = server.tmux_stdout(&["display-message", "-p", "-t", &target, "#S"]);
        let _viewing = ControlClient::attach(server.name(), &session);

        assert_left_alone(&server, &project);
    }

    #[test]
    fn an_agent_marked_background_with_no_waiter_is_re_armed() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::Background);

        let status = send(&server, &project);

        assert!(
            status.ends_with("\nRe-armed 'implementer' (its waiter for background work is gone)"),
            "{status}"
        );
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer"),
            None
        );
        server.wait_for_pane_text(&target, &format!("❯ {PROMPT}"));
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
    fn a_box_in_vim_normal_mode_is_pressed_no_key() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, name) = server.setup_project_with_feature(dir.path(), "login");
        let (_, received) = server.spawn_recording_agent(
            &project,
            &tmux::session_name(&name, "login"),
            "login",
            "implementer",
            Harness::ClaudeCode,
        );
        let waiting = Waiting::now(WaitingKind::HookEnded, None);
        runtime::write_waiting(&project, "login", "implementer", &waiting).unwrap();
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

        assert_left_alone(&server, &project);

        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(std::fs::read(&received).unwrap_or_default(), b"");
    }
}
