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
//! The prompt is posted to the session's inbound channel
//! ([`Harness::post_prompt`](crate::harness::Harness::post_prompt)), which
//! needs no keystroke, so neither the pane nor the input line matters: the
//! harness holds a post behind an open dialog and leaves a draft alone. A
//! session that reported no channel is an error before anything is
//! claimed, and so is a channel nothing listens on any more; either way the
//! agent stays unarmed until something prompts it.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig, resolve_harness_config};
use crate::state::runtime::{self, Waiting, WaitingClass, WaitingKind};
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
    let Some(inbound) = runtime::read_inbound(project_root, scope, agent) else {
        return Err(PmError::Agent(format!(
            "its session reported no wake channel, so nothing wakes it to read this until it \
             is prompted or restarted ({} starts a session that may report one)",
            super::agent_restart::restart_command(&project.project.name, scope, agent)
        )));
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
    let posted = hooks_stop::continuation(project_root, scope, agent)
        .and_then(|prompt| harness.post_prompt(&inbound, &prompt));
    match posted {
        Ok(()) => Ok(Some(waiting)),
        Err(e) => {
            let _ = if let Some(marker) = &marker {
                runtime::write_waiting(project_root, scope, agent, marker)
            } else {
                runtime::release_turn_end(project_root, scope, agent, entry)
            };
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_send::tests::agent_send;
    use crate::commands::running_agents::waiting;
    use crate::harness::Harness;
    use crate::testing::{FakeSocket, TestServer};
    use chrono::Utc;
    use tempfile::tempdir;

    const PROMPT: &str = "[pm message] From reviewer@login: \"hi\". Run `pm msg read` to read it. \
                          (pm delivers messages this way; this is not an error.)";

    /// Assert a send's `status` says it queued the message and re-armed
    /// nothing; returns the line on the recipient's state.
    fn left_alone(status: &str) -> String {
        let (sent, state) = status.split_once('\n').unwrap_or((status, ""));
        assert_eq!(sent, "Message 001 sent to 'implementer' (from 'reviewer')");
        assert!(!state.contains("re-armed"), "{status}");
        state.to_string()
    }

    /// A feature whose `implementer` sits at an empty Claude Code prompt
    /// with `marker` as its waiting marker.
    fn at_prompt(
        server: &TestServer,
        dir: &Path,
        marker: WaitingKind,
    ) -> (std::path::PathBuf, String) {
        let (project, _, project_name) = server.setup_project_with_feature(dir, "login");
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

    /// Record `socket` as the agent's session's inbound channel.
    fn listening(project: &Path, socket: &Path) {
        let inbound = runtime::Inbound {
            endpoint: runtime::Endpoint::Socket(socket.to_path_buf()),
            token: Some("t0k".into()),
            thread: None,
        };
        runtime::write_inbound(project, "login", "implementer", Some(&inbound)).unwrap();
    }

    /// A socket recorded as the agent's session's inbound channel.
    fn posting(dir: &Path, project: &Path) -> FakeSocket {
        let socket = FakeSocket::bind(dir);
        listening(project, &socket.path);
        socket
    }

    /// Assert the next post to `socket` is the agent's messages prompt.
    fn assert_posted_prompt(socket: &FakeSocket) {
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
    }

    #[test]
    fn an_unarmed_agent_with_a_socket_is_posted_its_prompt_whatever_its_pane_shows() {
        let server = TestServer::own("rearm-socket");
        let dir = tempdir().unwrap();
        let (project, target) = at_prompt(&server, dir.path(), WaitingKind::HookEnded);
        let socket = posting(dir.path(), &project);
        let pane = server.pane_id(&target);
        server.tmux_stdout(&["copy-mode", "-t", &pane]);

        let status = send(&server, &project);

        assert!(
            status.ends_with(
                "\n'implementer' was unarmed (Stop hook ended): re-armed, it reads this now"
            ),
            "{status}"
        );
        assert_posted_prompt(&socket);
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer"),
            None
        );
        assert!(tmux::paste::in_mode(server.name(), &pane).unwrap());
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
        let (project, _, project_name) = server.setup_project_with_feature(dir, "login");
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
            endpoint: runtime::Endpoint::Socket(app_server.path.clone()),
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

            assert!(status.contains("\n'implementer' was unarmed ("), "{status}");
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

    /// An idle opencode agent whose session `ses_1` is served at `url`
    /// with password `pw`, as its host's address file says. Returns the project and the file the pane's
    /// keystrokes go to.
    fn opencode_at_prompt(
        server: &TestServer,
        dir: &Path,
        url: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let (project, _, project_name) = server.setup_project_with_feature(dir, "login");
        let session = tmux::session_name(&project_name, "login");
        let (_, received) = server.spawn_recording_agent(
            &project,
            &session,
            "login",
            "implementer",
            Harness::OpenCode,
        );
        let idle = Waiting::now(WaitingKind::Idle, None);
        runtime::write_waiting(&project, "login", "implementer", &idle).unwrap();
        let file = dir.join("opencode-server.json");
        let address = serde_json::json!({"url": url, "password": "pw"});
        std::fs::write(&file, address.to_string()).unwrap();
        let inbound = runtime::Inbound {
            endpoint: runtime::Endpoint::ServerFile(file),
            token: None,
            thread: Some("ses_1".into()),
        };
        runtime::write_inbound(&project, "login", "implementer", Some(&inbound)).unwrap();
        (project, received)
    }

    #[test]
    fn an_unarmed_opencode_agent_is_prompted_through_its_server_never_typed() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let opencode = crate::testing::FakeOpenCodeServer::answering(200, "{}");
        let (project, received) = opencode_at_prompt(&server, dir.path(), &opencode.url);

        let status = send(&server, &project);

        assert!(status.contains("\n'implementer' was unarmed"), "{status}");
        let request = opencode.request().expect("a prompt");
        assert_eq!(request.path, "/api/session/ses_1/prompt");
        assert_eq!(request.body["text"], PROMPT);
        assert_eq!(std::fs::read_to_string(received).unwrap_or_default(), "");
    }

    #[test]
    fn an_opencode_server_that_is_gone_leaves_the_agent_unarmed_and_says_so() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let (project, received) = opencode_at_prompt(&server, dir.path(), &url);

        let err = rearm(&project, "login", "implementer", server.name()).unwrap_err();

        assert!(err.to_string().contains("could not reach"), "{err}");
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer").map(|w| w.kind),
            Some(WaitingKind::Idle)
        );
        assert_eq!(std::fs::read_to_string(received).unwrap_or_default(), "");
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

            left_alone(&send(&server, &project));
            assert_eq!(app_server.turn(), None);
        }
    }

    #[test]
    fn an_agent_asking_the_user_is_posted_nothing() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _) = at_prompt(&server, dir.path(), WaitingKind::Permission);
        let socket = posting(dir.path(), &project);

        left_alone(&send(&server, &project));
        assert!(!socket.posted());
    }

    #[test]
    fn a_socket_nothing_listens_on_leaves_the_agent_unarmed_and_says_so() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _) = at_prompt(&server, dir.path(), WaitingKind::HookEnded);
        listening(&project, &dir.path().join("gone.sock"));

        let err = rearm(&project, "login", "implementer", server.name()).unwrap_err();

        assert!(err.to_string().contains("gone.sock is gone"), "{err}");
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer").map(|w| w.kind),
            Some(WaitingKind::HookEnded)
        );
    }

    #[test]
    fn a_session_that_reported_no_channel_is_left_unarmed_and_untyped_with_a_warning() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _, name) = server.setup_project_with_feature(dir.path(), "login");
        let (_, received) = server.spawn_recording_agent(
            &project,
            &tmux::session_name(&name, "login"),
            "login",
            "implementer",
            Harness::ClaudeCode,
        );
        let waiting = Waiting::now(WaitingKind::HookEnded, None);
        runtime::write_waiting(&project, "login", "implementer", &waiting).unwrap();

        let sent = agent_send(
            &project,
            "login",
            None,
            "implementer",
            "reviewer",
            "hi",
            server.name(),
        )
        .unwrap();

        left_alone(&sent.status);
        assert_eq!(
            sent.warnings,
            [format!(
                "message 001 is queued for 'implementer@login', but it could not be re-armed: \
                 its session reported no wake channel, so nothing wakes it to read this until \
                 it is prompted or restarted (`pm agent restart implementer --scope login \
                 --project {name}` starts a session that may report one)"
            )]
        );
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer").map(|w| w.kind),
            Some(WaitingKind::HookEnded)
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(std::fs::read(&received).unwrap_or_default(), b"");
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
        let (project, _) = started_long_ago(&server, dir.path(), chrono::Duration::seconds(1));
        let socket = posting(dir.path(), &project);

        let state = left_alone(&send(&server, &project));
        assert_eq!(
            state,
            "'implementer' is busy: it reads this when its turn ends"
        );
        assert!(!socket.posted());
    }

    #[test]
    fn an_agent_whose_start_armed_no_waiter_is_re_armed_once() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _) = started_long_ago(&server, dir.path(), chrono::Duration::zero());
        let socket = posting(dir.path(), &project);

        let status = send(&server, &project);

        assert!(
            status.ends_with("\n'implementer' was unarmed (its session started, but armed no waiter): re-armed, it reads this now"),
            "{status}"
        );
        assert_posted_prompt(&socket);
        assert_eq!(
            send(&server, &project),
            "Message 002 sent to 'implementer' (from 'reviewer')\n'implementer' is busy: it \
             reads this when its turn ends"
        );
    }

    #[test]
    fn an_agent_marked_background_with_no_waiter_is_re_armed() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _) = at_prompt(&server, dir.path(), WaitingKind::Background);
        let socket = posting(dir.path(), &project);

        let status = send(&server, &project);

        assert!(
            status.ends_with("\n'implementer' was unarmed (its waiter for background work is gone): re-armed, it reads this now"),
            "{status}"
        );
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer"),
            None
        );
        assert_posted_prompt(&socket);
    }

    #[test]
    fn an_agent_asking_the_user_is_left_alone() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _) = at_prompt(&server, dir.path(), WaitingKind::Permission);
        let socket = posting(dir.path(), &project);

        let status = send(&server, &project);

        assert_eq!(
            left_alone(&status),
            "'implementer' is asking the user (permission prompt): it reads this once that is \
             answered"
        );
        assert!(!socket.posted());
    }

    #[test]
    fn a_loop_that_stopped_itself_is_left_alone() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _) = at_prompt(&server, dir.path(), WaitingKind::Tripped);
        let socket = posting(dir.path(), &project);

        let status = send(&server, &project);

        assert_eq!(
            left_alone(&status),
            "'implementer' is unarmed (never-idle loop stopped): nothing wakes it to read this \
             until it is prompted"
        );
        assert!(!socket.posted());
    }

    #[test]
    fn an_agent_interrupted_without_a_hook_is_re_armed_from_its_transcript() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, _) = at_prompt(&server, dir.path(), WaitingKind::Permission);
        let socket = posting(dir.path(), &project);
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
            status.ends_with(
                "\n'implementer' was unarmed (interrupted): re-armed, it reads this now"
            ),
            "{status}"
        );
        assert_eq!(
            waiting(&project, "login", "implementer", Harness::ClaudeCode),
            None,
            "a re-armed interrupt no longer counts"
        );
        assert_posted_prompt(&socket);

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
        let (project, _) = at_prompt(&server, dir.path(), WaitingKind::Dialog);
        let socket = posting(dir.path(), &project);
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
            status.ends_with(
                "\n'implementer' was unarmed (its waiter is gone): re-armed, it reads this now"
            ),
            "{status}"
        );
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer"),
            None
        );
        assert_posted_prompt(&socket);
    }
}
