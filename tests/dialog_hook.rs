//! The installed dialog hook command, run the way Claude Code runs it,
//! answered through the path `pm serve` answers it by.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pm::commands::hooks_dialog::{self, Answered};
use pm::commands::hooks_install::dialog_hook_command;
use pm::harness::Harness;
use pm::state::runtime::{self, ANSWER_CHOICE, Answer, Waiting, WaitingKind};
use serde_json::json;
use tempfile::tempdir;

const AGENT: &str = "implementer";

fn project(dir: &Path) {
    std::fs::create_dir_all(dir.join(".pm")).unwrap();
    std::fs::create_dir_all(dir.join("main")).unwrap();
}

fn path_with_pm() -> String {
    let bin = assert_cmd::cargo::cargo_bin("pm");
    format!(
        "{}:{}",
        bin.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn question(text: &str) -> serde_json::Value {
    json!({
        "hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
        "tool_input": {"questions": [{"question": text, "header": "Q", "multiSelect": false,
            "options": [{"label": "Yes", "description": ""}, {"label": "No", "description": ""}]}]}
    })
}

fn command(dir: &Path, shell: &str) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", shell])
        .env(
            "PM_TMUX_SERVER",
            format!("pm-test-{}-dialog-none", std::process::id()),
        )
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("HOME", dir)
        .env("PATH", path_with_pm())
        .env("PM_AGENT_NAME", AGENT)
        .current_dir(dir.join("main"));
    cmd
}

/// The installed command with `payload` on stdin, its marker written as the
/// waiting hook beside it would. Returns once its dialog is recorded.
fn spawn_hook(dir: &Path, payload: &serde_json::Value) -> (Child, String) {
    let mut hook = command(dir, &dialog_hook_command(Harness::ClaudeCode))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    hook.stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let id = recorded(dir, None);
    let waiting = Waiting::now(WaitingKind::Question, None);
    runtime::write_waiting(dir, "main", AGENT, &waiting).unwrap();
    (hook, id)
}

/// The id of the dialog recorded once it is not `previous`.
fn recorded(dir: &Path, previous: Option<&str>) -> String {
    let start = Instant::now();
    loop {
        if let Some(r) = runtime::read_dialog(dir, "main", AGENT)
            && Some(r.dialog.id.as_str()) != previous
        {
            return r.dialog.id;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "never recorded");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_exit(hook: &mut Child) -> std::process::Output {
    let start = Instant::now();
    while hook.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(10) {
            hook.kill().unwrap();
            panic!("hook still waiting");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = hook.stdout.take().map(|mut s| {
        let mut v = Vec::new();
        std::io::Read::read_to_end(&mut s, &mut v).unwrap();
        v
    });
    std::process::Output {
        status: hook.wait().unwrap(),
        stdout: out.unwrap_or_default(),
        stderr: Vec::new(),
    }
}

#[test]
fn an_answer_left_for_the_dialog_is_printed_as_the_decision() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let (mut hook, id) = spawn_hook(dir.path(), &question("Ship it?"));

    let answer = Answer {
        id: id.clone(),
        choice: ANSWER_CHOICE.into(),
        answers: [("Ship it?".to_string(), vec!["Yes".to_string()])].into(),
        message: None,
    };
    let answered = hooks_dialog::answer(
        dir.path(),
        "main",
        AGENT,
        Harness::ClaudeCode,
        &answer,
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(answered, Answered::Taken);

    let out = wait_for_exit(&mut hook);
    assert!(out.status.success());
    let decision: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        decision["hookSpecificOutput"]["decision"],
        json!({"behavior": "allow", "updatedInput": {
            "questions": question("Ship it?")["tool_input"]["questions"],
            "answers": {"Ship it?": "Yes"}}})
    );
    assert_eq!(runtime::read_dialog(dir.path(), "main", AGENT), None);
    assert!(!runtime::answer_pending(dir.path(), "main", AGENT));
}

#[test]
fn a_dialog_answered_at_the_terminal_ends_the_hook_silently() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let (mut hook, _) = spawn_hook(dir.path(), &question("Ship it?"));
    std::thread::sleep(Duration::from_millis(600));

    // PostToolUse, as the user answers at the terminal.
    runtime::clear_waiting(dir.path(), "main", AGENT).unwrap();
    let out = wait_for_exit(&mut hook);
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(runtime::read_dialog(dir.path(), "main", AGENT), None);
}

#[test]
fn a_newer_dialog_ends_the_older_ones_hook_and_keeps_its_record() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let (mut first, first_id) = spawn_hook(dir.path(), &question("First?"));
    let mut second = command(dir.path(), &dialog_hook_command(Harness::ClaudeCode))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    second
        .stdin
        .take()
        .unwrap()
        .write_all(question("Second?").to_string().as_bytes())
        .unwrap();
    let second_id = recorded(dir.path(), Some(&first_id));

    let out = wait_for_exit(&mut first);
    assert!(out.stdout.is_empty());
    let left = runtime::read_dialog(dir.path(), "main", AGENT).unwrap();
    assert_eq!(left.dialog.id, second_id);
    assert_eq!(
        hooks_dialog::current(dir.path(), "main", AGENT, Harness::ClaudeCode).map(|r| r.dialog.id),
        Some(second_id)
    );
    second.kill().unwrap();
    second.wait().unwrap();
}

#[test]
fn the_hook_ends_once_its_harness_is_gone() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let payload_file: PathBuf = dir.path().join("payload.json");
    std::fs::write(&payload_file, question("Ship it?").to_string()).unwrap();
    // A harness that runs the hook, then dies without killing it.
    let mut harness = command(
        dir.path(),
        &format!(
            "pm harness hooks dialog claude-code < '{}' > /dev/null & echo $!; wait",
            payload_file.display()
        ),
    )
    .stdout(Stdio::piped())
    .spawn()
    .unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(harness.stdout.as_mut().unwrap()),
        &mut line,
    )
    .unwrap();
    let hook: libc::pid_t = line.trim().parse().unwrap();
    recorded(dir.path(), None);
    runtime::write_waiting(
        dir.path(),
        "main",
        AGENT,
        &Waiting::now(WaitingKind::Question, None),
    )
    .unwrap();

    harness.kill().unwrap();
    harness.wait().unwrap();
    let start = Instant::now();
    // SAFETY: signal 0 only checks the process exists.
    while unsafe { libc::kill(hook, 0) } == 0 {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the hook outlived its harness"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(runtime::read_dialog(dir.path(), "main", AGENT), None);
}
