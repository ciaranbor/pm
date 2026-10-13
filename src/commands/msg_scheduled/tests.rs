use super::*;
use crate::commands::agent_send::tests::setup_project_with_tmux;
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::paths;
use crate::testing::TestServer;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn delays_parse_whole_units_and_compounds() {
    let secs = |text: &str| parse_delay(text).map(|d| d.num_seconds());
    assert_eq!(secs("90s"), Ok(90));
    assert_eq!(secs("10m"), Ok(600));
    assert_eq!(secs("1h30m"), Ok(5400));
    assert_eq!(secs("2d"), Ok(172_800));
    for text in ["90s", "1h30m", "2d", "1d2h3m4s"] {
        let canonical = if text == "90s" { "1m30s" } else { text };
        assert_eq!(delay_text(parse_delay(text).unwrap()), canonical);
    }
    for bad in ["", "0m", "10", "5x", "-1m", "m", "1h 30m"] {
        assert!(parse_delay(bad).is_err(), "{bad:?} parsed");
    }
}

/// A project with an active `reviewer` in scope `login`.
fn project(dir: &std::path::Path, server: &TestServer) -> PathBuf {
    let (root, session, feature) = setup_project_with_tmux(dir, server);
    server.spawn_fake_agent(&root, &session, &feature, "reviewer");
    root
}

fn from_main(root: &std::path::Path) -> Sender<'_> {
    Sender {
        name: "implementer",
        home: Some(Home {
            root,
            scope: "main",
        }),
        shell: false,
    }
}

fn unread(root: &std::path::Path) -> Vec<(String, u32)> {
    messages::check(&paths::messages_dir(root), "login", "reviewer")
        .unwrap()
        .into_iter()
        .map(|s| (s.sender, s.count))
        .collect()
}

#[test]
fn a_send_is_delivered_once_due_with_its_senders_home() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let root = project(dir.path(), &server);
    let delay = chrono::Duration::minutes(10);

    let send = schedule(&root, "login", "reviewer", from_main(&root), "hi", delay).unwrap();
    assert!(
        deliver_due(&root, Utc::now(), server.name())
            .lines
            .is_empty()
    );
    assert!(unread(&root).is_empty());

    let lines = deliver_due(&root, send.due, server.name()).lines;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(unread(&root), [("implementer@main".to_string(), 1)]);
    let message = messages::read_at(
        &paths::messages_dir(&root),
        "login",
        "reviewer",
        "implementer@main",
        1,
    )
    .unwrap()
    .unwrap();
    assert_eq!(message.body, "hi");
    assert_eq!(message.meta.sender_scope.as_deref(), Some("main"));
    assert!(scheduled::list(&root).unwrap().is_empty());
}

#[test]
fn a_send_scheduled_from_a_shell_is_delivered_with_no_reply() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let root = project(dir.path(), &server);
    let sender = Sender {
        name: "ciaran",
        shell: true,
        ..from_main(&root)
    };

    let send = schedule(
        &root,
        "login",
        "reviewer",
        sender,
        "later",
        chrono::Duration::minutes(1),
    )
    .unwrap();
    deliver_due(&root, send.due, server.name());

    let message = messages::read_at(
        &paths::messages_dir(&root),
        "login",
        "reviewer",
        "ciaran",
        1,
    )
    .unwrap()
    .unwrap();
    assert_eq!(message.body, "later");
    assert!(message.meta.from_shell);
    assert_eq!(message.meta.sender_scope, None);
}

#[test]
fn racing_sweeps_deliver_a_send_once() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let root = project(dir.path(), &server);
    let delay = chrono::Duration::seconds(1);
    let send = schedule(&root, "login", "reviewer", from_main(&root), "hi", delay).unwrap();

    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| deliver_due(&root, send.due, server.name()));
        }
    });

    assert_eq!(unread(&root), [("implementer@main".to_string(), 1)]);
}

#[test]
fn a_cancelled_send_is_never_delivered() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let root = project(dir.path(), &server);
    let delay = chrono::Duration::seconds(1);
    let send = schedule(&root, "login", "reviewer", from_main(&root), "hi", delay).unwrap();

    cancel(&root, &send.id[..3]).unwrap();

    assert!(deliver_due(&root, send.due, server.name()).lines.is_empty());
    assert!(unread(&root).is_empty());
}

#[test]
fn a_send_whose_recipient_left_fails_for_good_and_stays_listed() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let root = project(dir.path(), &server);
    let delay = chrono::Duration::seconds(1);
    let send = schedule(&root, "login", "reviewer", from_main(&root), "hi", delay).unwrap();
    let agents = paths::agents_dir(&root);
    let mut registry = AgentRegistry::load(&agents, "login").unwrap();
    registry.get_mut("reviewer").unwrap().active = false;
    registry.save(&agents, "login").unwrap();

    let lines = deliver_due(&root, send.due, server.name()).lines;
    assert!(lines[0].contains("failed"), "{lines:?}");
    assert!(deliver_due(&root, send.due, server.name()).lines.is_empty());

    let listed = list(&root).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].contains("failed: No active agent called 'reviewer'"));
    assert!(unread(&root).is_empty());
}

#[test]
fn scheduling_to_an_inactive_recipient_fails_as_a_send_would() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let delay = chrono::Duration::seconds(1);

    let err = schedule(root, "login", "qa", from_main(root), "hi", delay).unwrap_err();

    assert!(matches!(err, PmError::AgentNotFound(_)), "{err}");
    assert!(err.to_string().contains("No active agent called 'qa'"));
    assert!(scheduled::list(root).unwrap().is_empty());
}

#[test]
fn a_send_that_keeps_failing_is_failed_once_its_retries_time_out() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let root = project(dir.path(), &server);
    let delay = chrono::Duration::seconds(1);
    let send = schedule(&root, "login", "reviewer", from_main(&root), "hi", delay).unwrap();
    // The sender's home project is removed before the send falls due.
    let stored = paths::pm_dir(&root).join(format!("runtime/.scheduled/{}.json", send.id));
    let moved = Scheduled {
        home: Some(ScheduledHome {
            root: dir.path().join("gone"),
            scope: "main".into(),
        }),
        ..send.clone()
    };
    std::fs::write(&stored, serde_json::to_vec(&moved).unwrap()).unwrap();

    for late in [0, 1, RETRY_WINDOW.num_minutes() - 1] {
        let at = send.due + chrono::Duration::minutes(late);
        let swept = deliver_due(&root, at, server.name());
        assert!(swept.lines[0].contains("retried next sweep"), "{swept:?}");
    }
    let swept = deliver_due(&root, send.due + RETRY_WINDOW, server.name());
    assert!(swept.lines[0].contains("failed"), "{swept:?}");
    assert!(!swept.delivered);
    let later = send.due + RETRY_WINDOW * 2;
    assert!(deliver_due(&root, later, server.name()).lines.is_empty());
    assert!(scheduled::list(&root).unwrap()[0].failed.is_some());
    assert!(unread(&root).is_empty());
}
