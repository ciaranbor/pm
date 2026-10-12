use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn completions_generates_zsh_output() {
    pm().args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef pm"));
}

#[test]
fn completions_generates_bash_output() {
    pm().args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_pm"));
}

#[test]
fn version_is_the_cargo_version_with_build_metadata_off_a_release() {
    let out = pm().arg("--version").assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let version = stdout.trim().strip_prefix("pm ").unwrap();
    let pattern =
        regex::Regex::new(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$").unwrap();
    assert!(pattern.is_match(version), "{stdout}");
    assert!(
        version.starts_with(&format!("{}+", env!("CARGO_PKG_VERSION"))),
        "{version} is not Cargo.toml's version with build metadata"
    );
}

/// The binary under test in a throwaway home of its own, which lives as
/// long as the command.
struct Pm {
    cmd: Command,
    _home: tempfile::TempDir,
}

impl std::ops::Deref for Pm {
    type Target = Command;
    fn deref(&self) -> &Command {
        &self.cmd
    }
}

impl std::ops::DerefMut for Pm {
    fn deref_mut(&mut self) -> &mut Command {
        &mut self.cmd
    }
}

fn pm() -> Pm {
    let home = tempdir().unwrap();
    Pm {
        cmd: Command::from_std(std_pm(home.path())),
        _home: home,
    }
}

/// The binary under test, reaching none of the user's own pm state: `home`
/// for `HOME`, no XDG dirs, no tmux server anyone uses, and no `pm` on
/// `PATH`, so it is never the installed pm.
fn std_pm(home: &std::path::Path) -> std::process::Command {
    let mut pm = std::process::Command::new(assert_cmd::cargo::cargo_bin("pm"));
    pm.envs(no_tmux())
        .env("HOME", home)
        .env("PATH", path_without_pm())
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("PM_AGENT_NAME")
        .env_remove(pm::state::paths::AGENT_WORKTREE_ENV);
    for var in pm::state::dirs::XDG_VARS {
        pm.env_remove(var);
    }
    pm
}

fn path_without_pm() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::split_paths(&path).filter(|dir| !dir.join("pm").exists());
    std::env::join_paths(dirs).unwrap()
}

/// A tmux server no test starts, so the binary under test never writes to
/// the default server or the one the run is inside.
fn no_tmux() -> [(&'static str, String); 1] {
    [(
        "PM_TMUX_SERVER",
        format!("pm-test-{}-cli-none", std::process::id()),
    )]
}

#[test]
fn no_args_shows_help() {
    pm().assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn help_flag_shows_usage() {
    pm().arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Terminal-based project manager"));
}

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
    assert!(home.join(".agents/skills/pm/SKILL.md").is_file());
    assert!(home.join(".claude/skills/pm/SKILL.md").is_file());

    // `upgrade` installs the bundled workflows into the global tier;
    // `workflow list` lists them from outside a project.
    pm_home().args(["upgrade"]).assert().success();
    pm_home()
        .args(["workflow", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("solo"))
        .stdout(predicate::str::contains("[global, bundled]"));
}

#[test]
fn feat_subcommand_help() {
    pm().args(["feat", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Feature management"));
}

#[test]
fn harness_subcommands_have_help() {
    for sub in [
        "hooks", "skills", "agents", "settings", "migrate", "export", "import", "list", "probe",
    ] {
        pm().args(["harness", sub, "--help"]).assert().success();
    }
    pm().args(["harness", "hooks", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("stop"))
        .stdout(predicate::str::contains("session-start"));
}

#[test]
fn harness_flag_rejects_unsupported_harness() {
    pm().args(["harness", "settings", "list", "--harness", "aider"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "supported: claude-code, codex, opencode",
        ));
}

#[test]
fn settings_are_refused_for_the_other_harnesses() {
    for harness in ["codex", "opencode"] {
        pm().args(["harness", "settings", "list", "--harness", harness])
            .assert()
            .failure()
            .stderr(predicate::str::contains(format!(
                "{harness} has no per-feature settings files"
            )));
    }
}

#[test]
fn session_commands_run_for_the_other_harnesses() {
    let home = tempdir().unwrap();
    pm().current_dir(home.path())
        .env("HOME", home.path())
        .env_remove("CODEX_HOME")
        .args([
            "harness",
            "migrate",
            "--from",
            "/old/path",
            "--harness",
            "codex",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Nothing to migrate"));
}

#[test]
fn harness_list_marks_default() {
    pm().args(["harness", "list"])
        .assert()
        .success()
        .stdout("claude-code (default)\ncodex\nopencode\n");
}

#[test]
fn unknown_subcommand_fails() {
    pm().arg("nonexistent")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
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

/// Register a project `name` under `home`'s registry, rooted at
/// `<home>/<name>` with a main worktree and a `reviewer` agent in main.
fn registered_project(home: &std::path::Path, name: &str) -> std::path::PathBuf {
    use pm::state::project::{ProjectConfig, ProjectEntry, ProjectInfo};
    let root = home.join(name);
    std::fs::create_dir_all(root.join("main")).unwrap();
    std::fs::create_dir_all(root.join(".pm/agents")).unwrap();
    std::fs::create_dir_all(root.join(".pm/features")).unwrap();
    ProjectConfig {
        project: ProjectInfo {
            name: name.to_string(),
            max_features: None,
            fresh_base: None,
        },
        agents: Default::default(),
        harness: Default::default(),
    }
    .save(&root.join(".pm"))
    .unwrap();
    std::fs::write(
        root.join(".pm/agents/main.toml"),
        "[agents.reviewer]\ntype = \"agent\"\nsession_id = \"\"\nwindow_name = \"reviewer\"\n\
         active = false\nharness = \"claude-code\"\n",
    )
    .unwrap();
    ProjectEntry {
        root: root.to_string_lossy().into_owned(),
        main_branch: "main".to_string(),
        repo_url: None,
        state_remote: None,
    }
    .save(
        &pm::state::paths::dirs_under(home).config.join("projects"),
        name,
    )
    .unwrap();
    root
}

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
fn a_reply_by_no_agent_is_routed_back_from_the_inbox_it_came_from() {
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

    assert_eq!(
        route_of(&beta, "lead", "reviewer@main@alpha"),
        (Some("main".to_string()), Some("alpha".to_string()))
    );
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
