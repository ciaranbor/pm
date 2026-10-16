use super::*;

#[test]
fn register_non_git_repo_fails() {
    let dir = tempdir().unwrap();
    let not_a_repo = dir.path().join("not-a-repo");
    std::fs::create_dir(&not_a_repo).unwrap();

    pm().args(["register", &not_a_repo.to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Not a git repository"));
}

#[test]
fn register_nonexistent_path_fails() {
    pm().args(["register", "/tmp/definitely-does-not-exist-pm-test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn list_with_no_projects() {
    let dir = tempdir().unwrap();
    // Point HOME to an empty dir so global registry is empty
    pm().env("HOME", dir.path().to_string_lossy().as_ref())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("No projects"));
}

#[test]
fn asset_commands_install_into_the_global_tier() {
    // Every bundled asset installs under $HOME: the canonical store, the
    // harness projection, and the global workflow tier.
    let dir = tempdir().unwrap();
    let home = dir.path();
    // Run outside any pm project so only the global tier is in play.
    let pm_home = || {
        let mut c = pm();
        c.env("HOME", home.to_string_lossy().as_ref())
            .current_dir(home);
        c
    };

    pm_home()
        .args(["harness", "agents", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Installed Agent 'reviewer'"));
    assert!(home.join(".agents/agents/reviewer.md").is_file());
    assert!(home.join(".claude/agents/reviewer.md").is_file());

    // The old project/global split is gone: `--global` is now an error.
    pm_home()
        .args(["harness", "skills", "install", "--global"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
    pm_home()
        .args(["harness", "skills", "install"])
        .assert()
        .success();
    assert!(home.join(".agents/skills/messaging/SKILL.md").is_file());
    assert!(home.join(".claude/skills/messaging/SKILL.md").is_file());

    // `upgrade`, which installed releases run after installing a newer one,
    // upgrades with this pm when the release check fails, installing the
    // bundled workflows into the global tier; `workflow list` lists them
    // from outside a project.
    pm_home()
        .args(["upgrade"])
        .env("PM_RELEASES_URL", "http://127.0.0.1:9")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "could not check for a newer pm release",
        ));
    // pm 0.5.0 and earlier run this after installing a release.
    pm_home()
        .args(["upgrade", "--all"])
        .env("PM_RELEASES_URL", "http://127.0.0.1:9")
        .assert()
        .success();
    pm_home()
        .args(["workflow", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("solo"))
        .stdout(predicate::str::contains("[global, bundled]"));
}

#[test]
fn feat_delete_without_project_root_fails() {
    let dir = tempdir().unwrap();
    pm().current_dir(dir.path())
        .args(["feat", "delete", "somefeat"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn feat_merge_without_project_root_fails() {
    let dir = tempdir().unwrap();
    pm().current_dir(dir.path())
        .args(["feat", "merge", "somefeat"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn agent_list_outside_worktree_shows_helpful_error() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    // Create a pm project but run from project root (not main/ or a feature/)
    std::fs::create_dir(root.join(".pm")).unwrap();
    std::fs::create_dir(root.join("main")).unwrap();

    pm().current_dir(root)
        .args(["agent", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Not in a feature or main worktree",
        ));
}

#[test]
fn feat_review_without_project_root_fails() {
    let dir = tempdir().unwrap();
    pm().current_dir(dir.path())
        .args(["feat", "review", "42"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn hooks_install_outside_a_project_installs_at_the_user_level() {
    let dir = tempdir().unwrap();
    let home = dir.path();
    pm().env("HOME", home.to_string_lossy().as_ref())
        .current_dir(home)
        .args(["harness", "hooks", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Installed pm hooks in"));
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap())
            .unwrap();
    assert!(settings["hooks"]["Stop"].is_array(), "{settings}");
    assert!(settings["hooks"]["SessionStart"].is_array(), "{settings}");
}

#[test]
fn stop_hook_outside_a_project_lets_the_session_stop() {
    // A machine-wide hook reaching `pm` from an agent-named session that
    // isn't in a pm project must neither block nor error.
    let dir = tempdir().unwrap();
    pm().env("HOME", dir.path().to_string_lossy().as_ref())
        .env("PM_AGENT_NAME", "x")
        .current_dir(dir.path())
        .args(["harness", "hooks", "stop"])
        .assert()
        .success()
        .stdout("{}");
}

#[test]
fn stop_hook_waiting_on_an_empty_inbox_exits_when_its_harness_goes() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".pm")).unwrap();
    std::fs::create_dir_all(dir.path().join("main")).unwrap();
    let mut hook = std_pm(dir.path())
        .env("PM_AGENT_NAME", "implementer")
        .current_dir(dir.path().join("main"))
        .args(["harness", "hooks", "stop"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    std::thread::sleep(Duration::from_millis(500));
    assert!(
        hook.try_wait().unwrap().is_none(),
        "a hook whose harness is alive must keep waiting"
    );

    drop(hook.stdout.take());
    let start = Instant::now();
    while hook.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(10) {
            hook.kill().unwrap();
            panic!("hook still waiting after its stdout reader closed");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(hook.wait().unwrap().success());
}

#[test]
fn agent_list_and_fork_take_their_scope_from_the_flag() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".pm/agents")).unwrap();
    std::fs::create_dir(root.join("main")).unwrap();
    std::fs::write(
        root.join(".pm/agents/main.toml"),
        "[agents.reviewer]\ntype = \"agent\"\nsession_id = \"\"\nwindow_name = \"reviewer\"\n\
         active = false\nharness = \"claude-code\"\n",
    )
    .unwrap();

    pm().current_dir(root)
        .args(["agent", "list", "--scope", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Agents in 'main'"))
        .stdout(predicate::str::contains("reviewer (inactive"));
    pm().current_dir(root)
        .args(["agent", "fork", "reviewer", "copy", "--scope", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nope"))
        .stderr(predicate::str::contains("Not in a feature").not());
}
