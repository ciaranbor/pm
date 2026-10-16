use super::*;

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
