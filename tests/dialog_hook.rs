//! The installed dialog hook command, run the way Claude Code runs it,
//! answered through the path `pm serve` answers it by.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pm::commands::hooks_dialog::{self, Answered};
use pm::commands::hooks_install::{dialog_hook_command, stop_hook_command};
use pm::harness::Harness;
use pm::state::runtime::{self, ANSWER_CHOICE, Answer, WaitingKind};
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
        .env_remove(pm::state::paths::AGENT_WORKTREE_ENV)
        .current_dir(dir.join("main"));
    cmd
}

/// The installed command with `payload` on stdin. Returns once its dialog
/// is recorded.
fn spawn_hook(dir: &Path, payload: &serde_json::Value) -> (Child, String) {
    let known: Vec<String> = runtime::read_dialogs(dir, "main", AGENT)
        .into_iter()
        .map(|r| r.dialog.id)
        .collect();
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
    (hook, recorded(dir, &known))
}

/// The id of a dialog recorded that is not one of `known`.
fn recorded(dir: &Path, known: &[String]) -> String {
    let start = Instant::now();
    loop {
        if let Some(r) = runtime::read_dialogs(dir, "main", AGENT)
            .into_iter()
            .find(|r| !known.contains(&r.dialog.id))
        {
            return r.dialog.id;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "never recorded");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn bash(subagent: &str, command: &str) -> serde_json::Value {
    json!({"hook_event_name": "PermissionRequest", "agent_id": subagent,
           "tool_name": "Bash", "tool_input": {"command": command}})
}

fn send(dir: &Path, id: &str, choice: &str) -> Answered {
    let answer = Answer {
        id: id.into(),
        choice: choice.into(),
        answers: Default::default(),
        message: None,
    };
    hooks_dialog::answer(
        dir,
        "main",
        AGENT,
        Harness::ClaudeCode,
        &answer,
        Duration::from_secs(5),
        None,
    )
    .unwrap()
}

fn decision(out: &std::process::Output) -> serde_json::Value {
    let printed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    printed["hookSpecificOutput"]["decision"].clone()
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
        None,
    )
    .unwrap();
    assert_eq!(answered, Answered::Taken);

    let out = wait_for_exit(&mut hook);
    assert!(out.status.success());
    assert_eq!(
        decision(&out),
        json!({"behavior": "allow", "updatedInput": {
            "questions": question("Ship it?")["tool_input"]["questions"],
            "answers": {"Ship it?": "Yes"}}})
    );
    assert_eq!(runtime::read_dialog(dir.path(), "main", AGENT, &id), None);
    assert_eq!(send(dir.path(), &id, "decline"), Answered::Elsewhere);
}

#[test]
fn a_dialog_approved_at_the_terminal_ends_its_hook_silently_through_the_waiting_hook() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let (mut hook, id) = spawn_hook(dir.path(), &bash("a1", "touch a"));

    // The tool's PostToolUse, as the user approves it at the terminal.
    waiting_hook(
        dir.path(),
        &json!({"hook_event_name": "PostToolUse", "agent_id": "a1",
                "tool_name": "Bash", "tool_input": {"command": "touch a"}}),
    );

    ended_silently(&mut hook);
    assert_eq!(send(dir.path(), &id, "allow"), Answered::Elsewhere);
}

/// The installed waiting hook, run to completion with `payload` on stdin.
fn waiting_hook(dir: &Path, payload: &serde_json::Value) {
    let mut waiting = command(dir, "pm harness hooks waiting claude-code")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    waiting
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    assert!(waiting.wait().unwrap().success());
}

fn ended_silently(hook: &mut Child) {
    let out = wait_for_exit(hook);
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A question answered at the terminal, by a picked option or typed text:
/// either way Claude Code (2.1.292, live) sends its PostToolUse with the
/// answers added to the input, which still ends its hook and leaves the
/// agent busy.
#[test]
fn a_question_answered_at_the_terminal_ends_its_hook() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let asked = question("Ship it?");
    let (mut hook, id) = spawn_hook(dir.path(), &asked);
    let record = runtime::read_dialog(dir.path(), "main", AGENT, &id).unwrap();
    runtime::write_waiting(dir.path(), "main", AGENT, &record.dialog.waiting()).unwrap();
    let mut input = asked["tool_input"].clone();
    input["answers"] = json!({"Ship it?": "Only on Fridays"});
    input["annotations"] = json!({});
    waiting_hook(
        dir.path(),
        &json!({"hook_event_name": "PostToolUse", "tool_name": "AskUserQuestion",
                "tool_input": input, "tool_response": input}),
    );

    ended_silently(&mut hook);
    assert_eq!(send(dir.path(), &id, "decline"), Answered::Elsewhere);
    assert_eq!(runtime::read_waiting(dir.path(), "main", AGENT), None);
}

#[test]
fn the_turn_ending_closes_the_agents_own_question_and_keeps_a_subagents_asking() {
    let dir = tempdir().unwrap();
    project(dir.path());
    // The agent's own question, answered in a way no PostToolUse matches.
    let (mut own, own_id) = spawn_hook(dir.path(), &question("Ship it?"));
    let (mut subagents, _) = spawn_hook(dir.path(), &bash("a1", "touch a"));

    let mut waiter = command(dir.path(), &stop_hook_command(Harness::ClaudeCode))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    waiter
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"background_tasks":[{"id":"a1","status":"running"}]}"#)
        .unwrap();

    ended_silently(&mut own);
    assert_eq!(send(dir.path(), &own_id, "decline"), Answered::Elsewhere);
    let start = Instant::now();
    let marker = loop {
        if let Some(w) =
            runtime::read_waiting(dir.path(), "main", AGENT).filter(|w| w.between_turns)
        {
            break w;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "never waited");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(marker.kind, WaitingKind::Permission);
    assert_eq!(marker.subagent.as_deref(), Some("a1"));
    assert!(subagents.try_wait().unwrap().is_none(), "still asking");

    for child in [&mut waiter, &mut subagents] {
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

#[test]
fn dialogs_open_at_once_are_each_answered_by_their_own_hook() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let (mut a, a_id) = spawn_hook(dir.path(), &bash("a1", "touch a"));
    let (mut b, b_id) = spawn_hook(dir.path(), &bash("a2", "touch b"));
    let open = hooks_dialog::open_dialogs(dir.path(), "main", AGENT, Harness::ClaudeCode);
    assert_eq!(open.len(), 2);

    assert_eq!(send(dir.path(), &b_id, "deny"), Answered::Taken);
    assert_eq!(
        decision(&wait_for_exit(&mut b)),
        json!({"behavior": "deny", "interrupt": true})
    );
    let open = hooks_dialog::open_dialogs(dir.path(), "main", AGENT, Harness::ClaudeCode);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].dialog.id, a_id);

    assert_eq!(send(dir.path(), &a_id, "allow"), Answered::Taken);
    assert_eq!(
        decision(&wait_for_exit(&mut a)),
        json!({"behavior": "allow"})
    );
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
    let id = recorded(dir.path(), &[]);

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
    assert_eq!(runtime::read_dialog(dir.path(), "main", AGENT, &id), None);
}
