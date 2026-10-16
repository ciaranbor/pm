use super::*;

#[test]
fn a_named_project_is_acted_on_from_inside_another() {
    let home = tempdir().unwrap();
    let home = home.path();
    let alpha = registered_project(home, "alpha");
    let beta = registered_project(home, "beta");
    std::fs::write(alpha.join(".pm/features/login.toml"), "").unwrap();
    std::fs::create_dir_all(alpha.join(".pm/summaries")).unwrap();
    std::fs::write(
        alpha.join(".pm/summaries/login.md"),
        "Alpha's login summary.\n",
    )
    .unwrap();
    std::fs::remove_file(beta.join(".pm/agents/main.toml")).unwrap();
    let pm_in_beta = || {
        let mut pm = Command::from_std(std_pm(home));
        pm.current_dir(beta.join("main"));
        pm
    };

    pm_in_beta()
        .args(["agent", "list", "--project", "alpha", "--scope", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reviewer (inactive"));
    pm_in_beta()
        .args(["agent", "list", "--project", "alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Not in a worktree of project 'alpha'",
        ));
    pm_in_beta()
        .args(["feat", "summary", "show", "login", "--project", "alpha"])
        .assert()
        .success()
        .stdout("Alpha's login summary.\n");
    pm_in_beta()
        .args(["feat", "summary", "show", "--project", "alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("provide a feature name"));
}

#[test]
fn a_named_project_is_reached_from_outside_every_project() {
    let home = tempdir().unwrap();
    let home = home.path();
    let alpha = registered_project(home, "alpha");
    let beta = registered_project(home, "beta");
    std::fs::write(
        beta.join(".pm/agents/main.toml"),
        "[agents.lead]\ntype = \"agent\"\nsession_id = \"\"\nwindow_name = \"lead\"\n\
         active = true\nharness = \"claude-code\"\n",
    )
    .unwrap();
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
    let pm_at_home = || {
        let mut pm = Command::from_std(std_pm(home));
        pm.current_dir(home);
        pm
    };
    let as_reviewer = || {
        let mut pm = pm_at_home();
        pm.env("PM_AGENT_NAME", "reviewer");
        pm
    };

    as_reviewer()
        .args(["msg", "list", "--project", "alpha", "--scope", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lead"));
    as_reviewer()
        .args(["msg", "read", "--project", "alpha", "--scope", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello reviewer"));
    as_reviewer()
        .args([
            "msg",
            "reply",
            "--project",
            "alpha",
            "--scope",
            "main",
            "thanks",
        ])
        .assert()
        .success();
    let replies = pm::messages::list(
        &pm::state::paths::messages_dir(&beta),
        "main",
        "lead",
        Some("reviewer"),
    )
    .unwrap();
    assert_eq!(replies.len(), 1, "the reply reaches lead in beta");
    pm_at_home()
        .args(["msg", "send", "--project", "beta", "lead", "status?"])
        .assert()
        .success()
        .stdout(predicate::str::contains("outside every project"));
    pm_at_home()
        .args(["feat", "status", "--project", "alpha"])
        .assert()
        .success()
        .stdout("No features\n");
}

#[test]
fn a_named_project_off_this_machine_is_refused_and_global_flags_conflict() {
    let home = tempdir().unwrap();
    let home = home.path();
    let alpha = registered_project(home, "alpha");
    std::fs::remove_dir(alpha.join("main")).unwrap();

    Command::from_std(std_pm(home))
        .current_dir(home)
        .args(["feat", "list", "--project", "alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("run `pm restore alpha`"));
    Command::from_std(std_pm(home))
        .current_dir(home)
        .args(["msg", "send", "--project", "alpha", "reviewer", "hi"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("run `pm restore alpha`"));
    assert!(!alpha.join(".pm/messages").exists());
    for args in [
        &["feat", "list", "--all", "--project", "alpha"][..],
        &["feat", "status", "--all", "--project", "alpha"],
        &[
            "agent",
            "restart",
            "--all",
            "--global",
            "--project",
            "alpha",
        ],
    ] {
        Command::from_std(std_pm(home))
            .current_dir(home)
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

#[test]
fn a_status_set_in_a_named_project_records_only_that_projects_agent() {
    let home = tempdir().unwrap();
    let home = home.path();
    let alpha = registered_project(home, "alpha");
    let beta = registered_project(home, "beta");
    let block_alpha_as = |worktree: &std::path::Path| {
        Command::from_std(std_pm(home))
            .current_dir(worktree)
            .env("PM_AGENT_NAME", "implementer")
            .env(pm::state::paths::AGENT_WORKTREE_ENV, worktree)
            .args(["feat", "status", "blocked", "main", "-m", "which db?"])
            .args(["--project", "alpha"])
            .assert()
            .success();
        pm::state::scope::TeamStatus::load(&alpha, "main").unwrap()
    };

    let status = block_alpha_as(&beta.join("main"));
    assert_eq!(status.reason(), Some("which db?"));
    assert_eq!(
        status.agent(),
        None,
        "beta's agent is not alpha's to wait on"
    );

    let status = block_alpha_as(&alpha.join("main"));
    assert_eq!(status.agent(), Some("implementer"));
}
