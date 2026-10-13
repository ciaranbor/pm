//! Re-arms an agent no message can wake (unarmed, at its prompt) by giving
//! it the prompt its waiter would have given, so it reads its messages and
//! its next turn's end starts a waiter again.
//!
//! The marker must say unarmed — never asking, nor a loop that stopped
//! itself on purpose — and the window must run its harness with no live
//! waiter. Removing the marker is the claim to prompt, so of two concurrent
//! senders only one does; the prompt's UserPromptSubmit would clear it
//! anyway. A turn's end read from the transcript is claimed by its entry
//! instead ([`runtime::claim_turn_end`]). The claim comes before the
//! prompt, and is given back when none is given.
//!
//! The prompt is posted to the session's inbound channel when it reported
//! one ([`Harness::post_prompt`]), which needs no keystroke, so neither the
//! pane nor the input line matters: the harness holds a post behind an
//! open dialog and leaves a draft alone. A channel that nothing listens on
//! any more is an error, and the agent stays unarmed. A session with no
//! channel is typed into instead, and keystrokes reach whatever the window
//! shows, so then the pane must not be in use ([`tmux::panes::in_use`]: in
//! a mode, or in front of an attached client — never cancelled, since
//! either is the user's), and the harness must read its input line as empty
//! and taking text. No key is pressed to change the harness's own mode: a
//! box in vim NORMAL mode, like a dialog, reads as unknown and is left
//! alone. A draft is never cleared; when any check fails the message just
//! stays queued and the agent stays visibly unarmed.

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

/// Re-arm `agent` if it is unarmed. Returns the marker it was re-armed
/// from, or `None` when it was left alone.
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
    let prompted = match runtime::read_inbound(project_root, scope, agent) {
        Some(inbound) => hooks_stop::continuation(project_root, scope, agent)
            .and_then(|prompt| harness.post_prompt(&inbound, &prompt))
            .map(|()| true),
        None => type_prompt(project_root, scope, agent, harness, &pane.id, tmux_server),
    };
    match prompted {
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
    use crate::commands::agent_send::tests::agent_send;
    use crate::commands::running_agents::waiting;
    use crate::testing::{ControlClient, TestServer};
    use chrono::Utc;
    use tempfile::tempdir;

    const PROMPT: &str =
        "You have new messages from reviewer@login. Run `pm msg read` to read them.";

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

    /// Record `socket` as the agent's session's inbound channel.
    fn listening(project: &Path, socket: &Path) {
        let inbound = runtime::Inbound {
            socket: socket.to_path_buf(),
            token: Some("t0k".into()),
            thread: None,
        };
        runtime::write_inbound(project, "login", "implementer", Some(&inbound)).unwrap();
    }

    #[test]
    fn an_unarmed_agent_with_a_socket_is_posted_its_prompt_whatever_its_pane_shows() {
        let server = TestServer::own("rearm-socket");
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::HookEnded);
        let socket = crate::testing::FakeSocket::bind(dir.path());
        listening(&project, &socket.path);
        let pane = server.pane_id(&target);
        server.tmux_stdout(&["copy-mode", "-t", &pane]);

        let status = send(&server, &project);

        assert!(
            status.ends_with("\nRe-armed 'implementer' (Stop hook ended)"),
            "{status}"
        );
        let prompt = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": PROMPT},
        });
        assert_eq!(
            socket.post(),
            format!(
                "{}\n{prompt}\n",
                serde_json::json!({"type": "auth", "token": "t0k"})
            )
        );
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer"),
            None
        );
        assert!(tmux::paste::in_mode(server.name(), &pane).unwrap());
        assert!(!screen(&server, &target).contains("You have new messages"));
    }

    /// A codex agent at its composer with `marker`, its session's thread
    /// `t1` served by a fake app-server. Returns the project, the server,
    /// and the file the pane's keystrokes go to.
    fn codex_at_prompt(
        server: &TestServer,
        dir: &Path,
        marker: WaitingKind,
    ) -> (
        std::path::PathBuf,
        crate::testing::FakeAppServer,
        std::path::PathBuf,
    ) {
        let (project, project_name) = server.setup_project_with_feature(dir, "login");
        let session = tmux::session_name(&project_name, "login");
        let (_, received) = server.spawn_recording_agent(
            &project,
            &session,
            "login",
            "implementer",
            Harness::Codex,
        );
        runtime::write_waiting(
            &project,
            "login",
            "implementer",
            &Waiting::now(marker, None),
        )
        .unwrap();
        let app_server = crate::testing::FakeAppServer::bind(dir);
        let inbound = runtime::Inbound {
            socket: app_server.path.clone(),
            token: None,
            thread: Some("t1".into()),
        };
        runtime::write_inbound(&project, "login", "implementer", Some(&inbound)).unwrap();
        (project, app_server, received)
    }

    #[test]
    fn an_unarmed_codex_agent_is_started_a_turn_on_its_thread_never_typed() {
        for marker in [WaitingKind::Interrupted, WaitingKind::Idle] {
            let server = TestServer::new();
            let dir = tempdir().unwrap();
            let (project, app_server, received) = codex_at_prompt(&server, dir.path(), marker);

            let status = send(&server, &project);

            assert!(status.contains("\nRe-armed 'implementer'"), "{status}");
            assert_eq!(
                app_server.turn(),
                Some(serde_json::json!({
                    "threadId": "t1",
                    "input": [{"type": "text", "text": PROMPT}],
                }))
            );
            assert_eq!(std::fs::read_to_string(received).unwrap_or_default(), "");
        }
    }

    #[test]
    fn a_codex_agent_asking_or_whose_loop_stopped_is_started_nothing() {
        for marker in [WaitingKind::Permission, WaitingKind::Tripped] {
            let server = TestServer::new();
            let dir = tempdir().unwrap();
            let (project, app_server, _) = codex_at_prompt(&server, dir.path(), marker);
            if marker == WaitingKind::Tripped {
                runtime::trip_loop(&project, "login", "implementer", "wasted wakes").unwrap();
            }

            assert_eq!(
                send(&server, &project),
                "Message 001 sent to 'implementer' (from 'reviewer')"
            );
            assert_eq!(app_server.turn(), None);
        }
    }

    #[test]
    fn an_agent_asking_the_user_is_posted_nothing() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _) = at_prompt(&server, dir.path(), WaitingKind::Permission);
        let socket = crate::testing::FakeSocket::bind(dir.path());
        listening(&project, &socket.path);

        assert_eq!(
            send(&server, &project),
            "Message 001 sent to 'implementer' (from 'reviewer')"
        );
        assert!(!socket.posted());
    }

    #[test]
    fn a_socket_nothing_listens_on_leaves_the_agent_unarmed_and_says_so() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::HookEnded);
        listening(&project, &dir.path().join("gone.sock"));

        let err = rearm(&project, "login", "implementer", server.name()).unwrap_err();

        assert!(err.to_string().contains("gone.sock is gone"), "{err}");
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer").map(|w| w.kind),
            Some(WaitingKind::HookEnded)
        );
        assert!(!screen(&server, &target).contains("You have new messages"));
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

    /// An agent at its prompt with no marker, whose session started past
    /// the grace with its last activity at `active`.
    fn started_long_ago(
        server: &TestServer,
        dir: &Path,
        active: chrono::Duration,
    ) -> (std::path::PathBuf, String) {
        let (project, target) = at_prompt(server, dir, WaitingKind::Idle);
        runtime::clear_waiting(&project, "login", "implementer").unwrap();
        let started = Utc::now() - chrono::Duration::minutes(2);
        runtime::mark_started(&project, "login", "implementer").unwrap();
        std::fs::File::options()
            .append(true)
            .open(paths::pm_dir(&project).join("runtime/login/implementer/started"))
            .unwrap()
            .set_modified(started.into())
            .unwrap();
        runtime::set_activity(&project, "login", "implementer", started + active);
        (project, target)
    }

    #[test]
    fn an_agent_active_since_its_start_is_busy_and_left_alone() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = started_long_ago(&server, dir.path(), chrono::Duration::seconds(1));

        assert_eq!(
            send(&server, &project),
            "Message 001 sent to 'implementer' (from 'reviewer')"
        );
        assert!(!screen(&server, &target).contains(PROMPT));
    }

    #[test]
    fn an_agent_whose_start_armed_no_waiter_is_re_armed_once() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = started_long_ago(&server, dir.path(), chrono::Duration::zero());

        let status = send(&server, &project);

        assert!(
            status.ends_with("\nRe-armed 'implementer' (its session started, but armed no waiter)"),
            "{status}"
        );
        server.wait_for_pane_text(&target, &format!("❯ {PROMPT}"));
        assert_eq!(
            send(&server, &project),
            "Message 002 sent to 'implementer' (from 'reviewer')"
        );
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
    fn an_agent_whose_dialog_closed_between_turns_without_its_waiter_is_re_armed() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::Dialog);
        let mut dialog = runtime::read_waiting(&project, "login", "implementer").unwrap();
        dialog.since -= chrono::Duration::minutes(1);
        dialog.between_turns = true;
        runtime::write_waiting(&project, "login", "implementer", &dialog).unwrap();
        let transcript = dir.path().join("session.jsonl");
        let discarded = serde_json::json!({"type": "system", "subtype": "informational",
            "content": "Auto-mode proposal discarded — nothing was saved.",
            "timestamp": Utc::now().to_rfc3339()});
        std::fs::write(&transcript, format!("{discarded}\n")).unwrap();
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
            status.ends_with("\nRe-armed 'implementer' (its waiter is gone)"),
            "{status}"
        );
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer"),
            None
        );
        server.wait_for_pane_text(&target, &format!("❯ {PROMPT}"));
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
