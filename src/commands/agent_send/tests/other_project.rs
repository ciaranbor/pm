//! Sends and replies between projects: the recipient is checked and
//! healed in its own project, and a reply reaches the inbox the original
//! sender's waiter watches.

use super::*;
use crate::commands::agent_read::agent_read;
use crate::commands::msg_reply::msg_reply;
use crate::state::project::ProjectEntry;
use crate::state::runtime::{self, Waiting, WaitingKind};

fn home<'a>(root: &'a Path, scope: &'a str) -> Option<Home<'a>> {
    Some(Home { root, scope })
}

fn from<'a>(name: &'a str, home: Option<Home<'a>>) -> Sender<'a> {
    Sender { name, home }
}

fn name_of(root: &Path) -> String {
    ProjectConfig::load(&paths::pm_dir(root))
        .unwrap()
        .project
        .name
}

fn register(projects_dir: &Path, root: &Path) {
    ProjectEntry {
        root: root.to_string_lossy().into_owned(),
        main_branch: "main".to_string(),
        repo_url: None,
        state_remote: None,
    }
    .save(projects_dir, &name_of(root))
    .unwrap();
}

#[test]
fn send_to_an_inactive_agent_of_another_project_errors_naming_it() {
    let server = TestServer::new();
    let (dir1, dir2) = (tempdir().unwrap(), tempdir().unwrap());
    let alpha = setup_project_files(dir1.path(), "alpha");
    let (beta, _) = setup_named_project(dir2.path(), &server, "beta", "main");

    let err = send_as(
        &beta,
        "main",
        "reviewer",
        from("implementer", home(&alpha, "login")),
        "hi",
        server.name(),
    )
    .unwrap_err()
    .to_string();

    assert!(
        err.contains(&format!(
            "'reviewer' exists in scope 'main' of project '{}'",
            name_of(&beta)
        )),
        "{err}"
    );
    assert!(!err.contains("--project"), "{err}");
    assert_eq!(
        messages::unread_count(&paths::messages_dir(&beta), "main", "reviewer"),
        0
    );
}

#[test]
fn send_to_another_project_heals_a_dead_window_there() {
    let server = TestServer::new();
    let (dir1, dir2) = (tempdir().unwrap(), tempdir().unwrap());
    let alpha = setup_project_files(dir1.path(), "alpha");
    let (beta, session) = setup_named_project(dir2.path(), &server, "beta", "main");
    create_agent_definition(&beta, "reviewer");
    crate::commands::agent_spawn::agent_spawn(&beta, "main", "reviewer", None, None, server.name())
        .unwrap();
    tmux::kill_session(server.name(), &session).unwrap();
    tmux::create_session(server.name(), &session, &paths::main_worktree(&beta)).unwrap();

    let sent = send_as(
        &beta,
        "main",
        "reviewer",
        from("implementer", home(&alpha, "login")),
        "review this",
        server.name(),
    )
    .unwrap();

    assert_eq!(
        sent.status,
        format!(
            "Message 001 sent to 'reviewer@main' in project '{}' (from 'implementer@login' in \
             project 'alpha')",
            name_of(&beta)
        )
    );
    let heal = sent.heal.unwrap();
    assert_eq!(heal.root, beta);
    assert!(
        tmux::find_window(server.name(), &session, "reviewer")
            .unwrap()
            .is_some()
    );
    let msg = messages::read_at(
        &paths::messages_dir(&beta),
        "main",
        "reviewer",
        &format!("implementer@login@{}", project_name(&alpha)),
        1,
    )
    .unwrap()
    .unwrap();
    assert_eq!(msg.meta.sender_scope.as_deref(), Some("login"));
    assert_eq!(msg.meta.sender_project.as_deref(), Some("alpha"));
}

#[test]
fn send_to_another_project_rearms_an_unarmed_agent() {
    let server = TestServer::own("xproj-rearm");
    let (dir1, dir2) = (tempdir().unwrap(), tempdir().unwrap());
    let alpha = setup_project_files(dir1.path(), "alpha");
    let (beta, session) = setup_named_project(dir2.path(), &server, "beta", "main");
    let target = server.spawn_prompting_fake_agent(&beta, &session, "main", "implementer");
    server.wait_for_pane_text(&target, "❯");
    let unarmed = Waiting::now(WaitingKind::HookEnded, None);
    runtime::write_waiting(&beta, "main", "implementer", &unarmed).unwrap();

    let status = send_as(
        &beta,
        "main",
        "implementer",
        from("reviewer", home(&alpha, "login")),
        "hi",
        server.name(),
    )
    .unwrap()
    .status;

    assert!(
        status.ends_with("\nRe-armed 'implementer' (Stop hook ended)"),
        "{status}"
    );
}

#[test]
fn a_reply_reaches_the_inbox_the_original_sender_watches_both_ways() {
    let server = TestServer::new();
    let (dir1, dir2, registry) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
    let (alpha, alpha_session) = setup_named_project(dir1.path(), &server, "alpha", "login");
    let (beta, beta_session) = setup_named_project(dir2.path(), &server, "beta", "main");
    register(registry.path(), &alpha);
    register(registry.path(), &beta);
    server.spawn_fake_agent(&alpha, &alpha_session, "login", "implementer");
    server.spawn_fake_agent(&beta, &beta_session, "main", "reviewer");

    send_as(
        &beta,
        "main",
        "reviewer",
        from("implementer", home(&alpha, "login")),
        "please review",
        server.name(),
    )
    .unwrap();
    agent_read(&beta, "main", "reviewer", None, None).unwrap();
    msg_reply(
        registry.path(),
        &beta,
        "main",
        "reviewer",
        home(&beta, "main"),
        "looks good",
        server.name(),
    )
    .unwrap();

    let alpha_messages = paths::messages_dir(&alpha);
    assert_eq!(
        messages::unread_count(&alpha_messages, "login", "implementer"),
        1
    );
    let read = agent_read(&alpha, "login", "implementer", None, None).unwrap();
    assert_eq!(read[1], "looks good");
    assert!(read.iter().any(|l| l.starts_with("Reply: ")), "{read:?}");

    msg_reply(
        registry.path(),
        &alpha,
        "login",
        "implementer",
        home(&alpha, "login"),
        "thanks",
        server.name(),
    )
    .unwrap();
    let back = messages::read_at(
        &paths::messages_dir(&beta),
        "main",
        "reviewer",
        &format!("implementer@login@{}", project_name(&alpha)),
        2,
    )
    .unwrap()
    .unwrap();
    assert_eq!(back.body, "thanks");
}

#[test]
fn a_reply_from_another_projects_inbox_routes_back_to_the_repliers_home() {
    let server = TestServer::new();
    let (dir1, dir2, registry) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
    let alpha = setup_project_files(dir1.path(), "alpha");
    let (beta, session) = setup_named_project(dir2.path(), &server, "beta", "main");
    server.spawn_fake_agent(&beta, &session, "main", "reviewer");
    let beta_messages = paths::messages_dir(&beta);
    messages::send(
        &beta_messages,
        "main",
        "implementer",
        "reviewer",
        "question",
    )
    .unwrap();
    agent_read(&beta, "main", "implementer", None, None).unwrap();

    msg_reply(
        registry.path(),
        &beta,
        "main",
        "implementer",
        home(&alpha, "login"),
        "answer",
        server.name(),
    )
    .unwrap();

    let reply = messages::read_at(
        &beta_messages,
        "main",
        "reviewer",
        &format!("implementer@login@{}", project_name(&alpha)),
        1,
    )
    .unwrap()
    .unwrap();
    assert_eq!(reply.meta.sender_scope.as_deref(), Some("login"));
    assert_eq!(reply.meta.sender_project.as_deref(), Some("alpha"));
}

#[test]
fn a_send_from_outside_every_project_cannot_be_replied_to() {
    let server = TestServer::new();
    let (dir, registry) = (tempdir().unwrap(), tempdir().unwrap());
    let (beta, session) = setup_named_project(dir.path(), &server, "beta", "main");
    server.spawn_fake_agent(&beta, &session, "main", "reviewer");

    let status = send_as(
        &beta,
        "main",
        "reviewer",
        from("ciaran", None),
        "hello",
        server.name(),
    )
    .unwrap()
    .status;
    assert!(status.contains("outside every project"), "{status}");

    let read = agent_read(&beta, "main", "reviewer", None, None).unwrap();
    assert!(
        read[0].contains("sent from outside every project: no one to reply to"),
        "{read:?}"
    );
    assert!(!read.iter().any(|l| l.starts_with("Reply: ")), "{read:?}");
    let err = msg_reply(
        registry.path(),
        &beta,
        "main",
        "reviewer",
        home(&beta, "main"),
        "thanks",
        server.name(),
    )
    .unwrap_err();
    assert!(matches!(err, PmError::Messaging(_)), "{err}");
}

#[test]
fn a_home_spelled_differently_from_the_target_root_is_the_same_project() {
    let server = TestServer::new();
    let dir = tempdir().unwrap();
    let (root, session, feature) = setup_project_with_tmux(dir.path(), &server);
    server.spawn_fake_agent(&root, &session, &feature, "reviewer");
    let spelled = root.join("main/..");

    let sent = send_as(
        &root,
        &feature,
        "reviewer",
        from("implementer", home(&spelled, &feature)),
        "hello",
        server.name(),
    )
    .unwrap();

    assert_eq!(
        sent.status,
        "Message 001 sent to 'reviewer' (from 'implementer')"
    );
}

fn project_name(root: &std::path::Path) -> String {
    crate::state::project::ProjectConfig::load(&paths::pm_dir(root))
        .unwrap()
        .project
        .name
}
