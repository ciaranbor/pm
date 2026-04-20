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
fn feat_subcommand_help() {
    pm().args(["feat", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Feature management"));
}

#[test]
fn claude_subcommand_help() {
    pm().args(["claude", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("settings"))
        .stdout(predicate::str::contains("skills"));
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
