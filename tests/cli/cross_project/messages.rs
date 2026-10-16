use super::*;

/// Register `agents` as active in `scope` of the project at `root`.
fn activate(root: &std::path::Path, scope: &str, agents: &[&str]) {
    let entries: String = agents
        .iter()
        .map(|a| {
            format!(
                "[agents.{a}]\ntype = \"agent\"\nsession_id = \"\"\nwindow_name = \"{a}\"\n\
                 active = true\nharness = \"claude-code\"\n"
            )
        })
        .collect();
    std::fs::write(root.join(format!(".pm/agents/{scope}.toml")), entries).unwrap();
}

/// The scope and project a reply from the `sender` address recorded as its
/// route back.
fn route_of(
    root: &std::path::Path,
    recipient: &str,
    sender: &str,
) -> (Option<String>, Option<String>) {
    let msg = pm::messages::read_at(
        &pm::state::paths::messages_dir(root),
        "main",
        recipient,
        sender,
        1,
    )
    .unwrap()
    .unwrap();
    (msg.meta.sender_scope, msg.meta.sender_project)
}

#[test]
fn an_agent_replying_from_another_projects_inbox_is_answered_at_home() {
    let home = tempdir().unwrap();
    let home = home.path();
    let alpha = registered_project(home, "alpha");
    let beta = registered_project(home, "beta");
    std::fs::write(alpha.join(".pm/features/login.toml"), "").unwrap();
    std::fs::create_dir_all(alpha.join("login")).unwrap();
    activate(&beta, "main", &["lead"]);
    pm::messages::send(
        &pm::state::paths::messages_dir(&beta),
        "main",
        "implementer",
        "lead",
        "question",
    )
    .unwrap();
    let as_implementer = |args: &[&str]| {
        Command::from_std(std_pm(home))
            .current_dir(alpha.join("login"))
            .env("PM_AGENT_NAME", "implementer")
            .env(pm::state::paths::AGENT_WORKTREE_ENV, alpha.join("login"))
            .args(args)
            .args(["--project", "beta", "--scope", "main"])
            .assert()
            .success();
    };

    as_implementer(&["msg", "read"]);
    as_implementer(&["msg", "reply", "answer"]);

    assert_eq!(
        route_of(&beta, "lead", "implementer@login@alpha"),
        (Some("login".to_string()), Some("alpha".to_string()))
    );
}

#[test]
fn a_reply_by_no_agent_reaches_the_sender_but_has_no_reply_itself() {
    let home = tempdir().unwrap();
    let home = home.path();
    let alpha = registered_project(home, "alpha");
    let beta = registered_project(home, "beta");
    activate(&beta, "main", &["lead"]);
    pm::messages::send_full(
        &pm::state::paths::messages_dir(&alpha),
        "main",
        "reviewer",
        "lead",
        "hello reviewer",
        Some("main"),
        Some("beta"),
    )
    .unwrap();
    let in_beta = |args: &[&str]| {
        Command::from_std(std_pm(home))
            .current_dir(beta.join("main"))
            .env("USER", "reviewer")
            .args(args)
            .args(["--project", "alpha", "--scope", "main"])
            .assert()
            .success();
    };

    in_beta(&["msg", "read"]);
    in_beta(&["msg", "reply", "thanks"]);

    // From a shell, so it has no route back of its own.
    let reply = pm::messages::read_at(
        &pm::state::paths::messages_dir(&beta),
        "main",
        "lead",
        "reviewer",
        1,
    )
    .unwrap()
    .unwrap();
    assert_eq!(reply.body, "thanks");
    assert!(reply.meta.from_shell);
}

#[test]
fn a_send_naming_its_own_project_defaults_to_its_own_scope() {
    let home = tempdir().unwrap();
    let home = home.path();
    let alpha = registered_project(home, "alpha");
    std::fs::write(alpha.join(".pm/features/login.toml"), "").unwrap();
    std::fs::create_dir_all(alpha.join("login")).unwrap();
    activate(&alpha, "login", &["reviewer"]);

    Command::from_std(std_pm(home))
        .current_dir(alpha.join("login"))
        .env("PM_AGENT_NAME", "implementer")
        .args(["msg", "send", "--project", "alpha"])
        .args(["reviewer", "hi"])
        .assert()
        .success()
        .stdout("Message 001 sent to 'reviewer' (from 'implementer')\n");
}
