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

fn pm() -> Command {
    Command::cargo_bin("pm").unwrap()
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

    // Workflow commands work outside a project, listing the global tier.
    pm_home().args(["workflow", "install"]).assert().success();
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
fn claude_is_a_hidden_alias_for_harness() {
    // The alias is not listed as a subcommand (help text may still mention
    // claude-code elsewhere).
    pm().arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("\n  harness "))
        .stdout(predicate::str::is_match(r"(?m)^\s+claude\s").unwrap().not());
    for path in [["claude", "hooks"], ["claude", "skills"]] {
        pm().args(path).arg("--help").assert().success();
    }
}

#[test]
fn harness_flag_rejects_unsupported_harness() {
    pm().args(["harness", "settings", "list", "--harness", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("supported: claude-code"));
}

#[test]
fn harness_list_marks_default() {
    pm().args(["harness", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-code (default)"));
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
