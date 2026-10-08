//! The installed Stop hook command, run the way a harness runs it: the
//! blocking hook opencode's plugin runs, against signals from the harness
//! and from anyone else, and Claude Code's waiter.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pm::commands::hooks_install::stop_hook_command;
use pm::harness::Harness;
use pm::state::paths;
use pm::state::runtime::{self, WaitingKind};
use tempfile::tempdir;

const AGENT: &str = "implementer";

fn project(dir: &Path) {
    std::fs::create_dir_all(dir.join(".pm")).unwrap();
    std::fs::create_dir_all(dir.join("main")).unwrap();
}

/// The blocking hook under `/bin/sh -c`, with the built `pm` first on
/// `PATH`, for an agent spawned in the main worktree. Returns once the hook
/// is about to wait.
fn spawn_hook(dir: &Path) -> Child {
    spawn_hook_in(dir, &dir.join("main"))
}

/// [`spawn_hook`], run in `cwd`, wherever the agent's shell has `cd`'d.
fn spawn_hook_in(dir: &Path, cwd: &Path) -> Child {
    let hook = start_in(dir, cwd, &stop_hook_command(Harness::OpenCode), "");
    // The stamp is touched after the signal handlers are installed.
    let start = Instant::now();
    while runtime::last_activity(dir, "main", AGENT).is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "hook never waited"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    hook
}

/// `command` under `/bin/sh -c`, as a harness runs a hook, with the built
/// `pm` first on `PATH`, and an empty payload.
fn start(dir: &Path, command: &str) -> Child {
    start_with(dir, command, "")
}

/// [`start`] with `payload` on stdin.
fn start_with(dir: &Path, command: &str, payload: &str) -> Child {
    start_in(dir, &dir.join("main"), command, payload)
}

/// [`start_with`], run in `cwd`.
fn start_in(dir: &Path, cwd: &Path, command: &str, payload: &str) -> Child {
    let bin = assert_cmd::cargo::cargo_bin("pm");
    // `stubs` holds stand-ins for harness CLIs a test needs.
    let path = format!(
        "{}:{}:{}",
        dir.join("stubs").display(),
        bin.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut hook = Command::new("/bin/sh");
    for var in pm::state::dirs::XDG_VARS {
        hook.env_remove(var);
    }
    let mut hook = hook
        .args(["-c", command])
        .env(
            "PM_TMUX_SERVER",
            format!("pm-test-{}-stop-none", std::process::id()),
        )
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("HOME", dir)
        .env("PATH", path)
        .env("PM_AGENT_NAME", AGENT)
        .env(paths::AGENT_WORKTREE_ENV, dir.join("main"))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = hook.stdin.take().unwrap();
    std::io::Write::write_all(&mut stdin, payload.as_bytes()).unwrap();
    hook
}

fn wait_for_exit(hook: &mut Child) {
    let start = Instant::now();
    while hook.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(10) {
            hook.kill().unwrap();
            panic!("hook still waiting");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_signal_from_another_process_leaves_the_hook_armed() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let mut hook = spawn_hook(dir.path());

    // A foreign sender, aimed at this hook's pid only: tests run alongside
    // real agents, whose hooks a pattern would also hit.
    let killed = Command::new("/bin/kill")
        .args(["-TERM", &hook.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        hook.try_wait().unwrap().is_none(),
        "a stray SIGTERM must not end the hook"
    );

    pm::messages::send(
        &paths::messages_dir(dir.path()),
        "main",
        AGENT,
        "researcher",
        "hi",
    )
    .unwrap();
    wait_for_exit(&mut hook);
    let out = hook.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let decision: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(decision["decision"], "block");
    assert_eq!(
        decision["reason"],
        "You have new messages from researcher. Run `pm msg read` to read them."
    );
}

#[test]
fn the_hook_finds_its_agent_wherever_the_agents_shell_has_moved() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let elsewhere = paths::summaries_dir(dir.path());
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut hook = spawn_hook_in(dir.path(), &elsewhere);

    pm::messages::send(
        &paths::messages_dir(dir.path()),
        "main",
        AGENT,
        "researcher",
        "hi",
    )
    .unwrap();
    wait_for_exit(&mut hook);
    let out = hook.wait_with_output().unwrap();
    let decision: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(decision["decision"], "block", "{out:?}");
}

#[test]
fn a_signal_from_the_harness_ends_the_hook_and_says_why() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let mut hook = spawn_hook(dir.path());

    // This test process is the hook's harness: its parent.
    // SAFETY: signalling a child this test owns.
    assert_eq!(
        unsafe { libc::kill(hook.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    wait_for_exit(&mut hook);
    let out = hook.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer.get("decision"), None, "{answer}");
    let message = answer["systemMessage"].as_str().unwrap();
    assert!(
        message.contains(&format!(
            "Stop hook ended by SIGTERM from pid {}",
            std::process::id()
        )),
        "{message}"
    );
    let waiting = runtime::read_waiting(dir.path(), "main", AGENT).unwrap();
    assert_eq!(waiting.kind, WaitingKind::HookEnded);
}

fn send(dir: &Path) {
    pm::messages::send(&paths::messages_dir(dir), "main", AGENT, "researcher", "hi").unwrap();
}

fn kind(dir: &Path) -> Option<WaitingKind> {
    runtime::read_waiting(dir, "main", AGENT).map(|w| w.kind)
}

/// Claude Code's waiter, run as Claude Code runs it.
fn waiter(dir: &Path) -> Child {
    start(dir, &stop_hook_command(Harness::ClaudeCode))
}

/// Once `dir`'s agent reads idle with `hook` as its waiter.
fn await_idle(dir: &Path, hook: &Child) {
    let start = Instant::now();
    while kind(dir) != Some(WaitingKind::Idle)
        || runtime::read_waiter(dir, "main", AGENT) != Some(hook.id())
    {
        assert!(start.elapsed() < Duration::from_secs(10), "never waited");
        std::thread::sleep(Duration::from_millis(20));
    }
}

const CONTINUATION: &str = "You have new messages from researcher. Run `pm msg read` to read them.";

#[test]
fn a_waiter_with_messages_unread_wakes_the_agent_at_once() {
    let dir = tempdir().unwrap();
    project(dir.path());
    send(dir.path());

    let mut hook = waiter(dir.path());
    wait_for_exit(&mut hook);
    let out = hook.wait_with_output().unwrap();

    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), CONTINUATION);
    assert!(out.stdout.is_empty());
    assert_eq!(kind(dir.path()), None);
}

#[test]
fn a_waiter_marks_the_agent_idle_until_a_message_wakes_it() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let mut hook = waiter(dir.path());
    await_idle(dir.path(), &hook);

    send(dir.path());
    wait_for_exit(&mut hook);
    let out = hook.wait_with_output().unwrap();

    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), CONTINUATION);
    assert_eq!(kind(dir.path()), None, "busy with the continuation");
}

/// The restart a sweep deferred runs as the agent goes idle, in a
/// process of its own, which drops a restart that is no longer due: here
/// the project has no config to launch the agent from.
#[test]
fn a_waiter_going_idle_runs_the_deferred_restart_of_a_marked_agent() {
    let dir = tempdir().unwrap();
    project(dir.path());
    runtime::mark_restart_at_idle(dir.path(), "main", AGENT).unwrap();

    let mut hook = waiter(dir.path());
    await_idle(dir.path(), &hook);
    let start = Instant::now();
    while runtime::restart_at_idle_marked(dir.path(), "main", AGENT) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the deferred restart never ran"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let log = std::fs::read_to_string(
        dir.path()
            .join(".pm/runtime/main/implementer/stop-hook.log"),
    )
    .unwrap();
    assert!(log.contains("restart at idle: dropped"), "{log}");

    send(dir.path());
    wait_for_exit(&mut hook);
    assert_eq!(
        hook.wait().unwrap().code(),
        Some(2),
        "it still wakes the agent"
    );
}

#[test]
fn a_superseded_waiter_ends_without_a_word() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let mut older = waiter(dir.path());
    await_idle(dir.path(), &older);
    let mut newer = waiter(dir.path());
    await_idle(dir.path(), &newer);

    wait_for_exit(&mut older);
    let out = older.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");

    send(dir.path());
    wait_for_exit(&mut newer);
    assert_eq!(newer.wait().unwrap().code(), Some(2));
}

#[test]
fn a_waiter_whose_harness_is_gone_ends_and_marks_nothing() {
    let dir = tempdir().unwrap();
    project(dir.path());
    // A harness that runs the waiter, then exits.
    let command = stop_hook_command(Harness::ClaudeCode).replace('\'', "'\\''");
    let mut harness = start(dir.path(), &format!("/bin/sh -c '{command}' & sleep 2"));
    let start = Instant::now();
    let pid = loop {
        if kind(dir.path()) == Some(WaitingKind::Idle)
            && let Some(pid) = runtime::read_waiter(dir.path(), "main", AGENT)
        {
            break pid;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "never waited");
        std::thread::sleep(Duration::from_millis(20));
    };
    let marker = runtime::read_waiting(dir.path(), "main", AGENT);
    harness.wait().unwrap();

    let start = Instant::now();
    // SAFETY: signal 0 only checks the waiter exists.
    while unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the waiter outlived its harness"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(runtime::read_waiting(dir.path(), "main", AGENT), marker);
}

#[test]
fn wakes_that_never_drain_the_inbox_stop_the_loop_and_say_so() {
    let dir = tempdir().unwrap();
    project(dir.path());
    send(dir.path());
    let run = || {
        let mut hook = waiter(dir.path());
        wait_for_exit(&mut hook);
        hook.wait_with_output().unwrap()
    };

    for _ in 0..5 {
        let out = run();
        assert_eq!(String::from_utf8_lossy(&out.stderr), CONTINUATION);
    }
    let out = run();
    assert_eq!(out.status.code(), Some(2));
    let notice = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        notice.starts_with("pm: never-idle loop stopped: 5 consecutive wakes"),
        "{notice}"
    );
    assert!(runtime::loop_tripped(dir.path(), "main", AGENT).is_some());

    let out = run();
    assert_eq!(out.status.code(), Some(0), "a stopped loop wakes no one");
    assert!(out.stderr.is_empty());
}

#[test]
fn a_wake_after_a_read_is_not_wasted() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let messages = paths::messages_dir(dir.path());
    for _ in 0..8 {
        send(dir.path());
        send(dir.path());
        let mut hook = waiter(dir.path());
        wait_for_exit(&mut hook);
        let out = hook.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stderr), CONTINUATION);
        pm::messages::next(&messages, "main", AGENT, "researcher").unwrap();
    }
    assert_eq!(runtime::loop_tripped(dir.path(), "main", AGENT), None);
}

#[test]
fn a_background_task_the_payload_lists_never_keeps_a_message_from_waking_the_agent() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let payload = r#"{"background_tasks":[{"id":"bash_7","status":"running"}],"session_crons":[]}"#;
    let mut hook = start_with(dir.path(), &stop_hook_command(Harness::ClaudeCode), payload);
    let start = Instant::now();
    while kind(dir.path()) != Some(WaitingKind::Background) {
        assert!(start.elapsed() < Duration::from_secs(10), "never waited");
        std::thread::sleep(Duration::from_millis(20));
    }

    send(dir.path());
    wait_for_exit(&mut hook);
    let out = hook.wait_with_output().unwrap();

    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), CONTINUATION);
    let log = std::fs::read_to_string(
        dir.path()
            .join(".pm/runtime/main")
            .join(AGENT)
            .join("stop-hook.log"),
    )
    .unwrap();
    assert!(log.contains("background_tasks=[bash_7:running]"), "{log}");
}

#[test]
fn a_subagents_dialog_open_as_the_turn_ends_keeps_the_agent_asking() {
    let dir = tempdir().unwrap();
    project(dir.path());
    let ask = serde_json::json!({"hook_event_name": "PermissionRequest", "agent_id": "a1",
                                 "tool_name": "Bash", "tool_input": {"command": "touch a"}});
    let (dialog, reply_context) = Harness::ClaudeCode.dialog(&ask).unwrap();
    let record = runtime::DialogRecord {
        dialog,
        // This process stands in for the dialog's hook.
        pid: Some(std::process::id()),
        reply_context,
    };
    runtime::write_dialog(dir.path(), "main", AGENT, &record).unwrap();

    let payload = r#"{"background_tasks":[{"id":"a1","status":"running"}],"session_crons":[]}"#;
    let mut hook = start_with(dir.path(), &stop_hook_command(Harness::ClaudeCode), payload);
    let start = Instant::now();
    while runtime::read_waiter(dir.path(), "main", AGENT).is_none() || kind(dir.path()).is_none() {
        assert!(start.elapsed() < Duration::from_secs(10), "never waited");
        std::thread::sleep(Duration::from_millis(20));
    }
    let marker = runtime::read_waiting(dir.path(), "main", AGENT).unwrap();
    assert_eq!(marker.kind, WaitingKind::Permission, "not background");
    assert_eq!(marker.subagent.as_deref(), Some("a1"));
    assert_eq!(marker.describe(), "Bash: touch a");

    hook.kill().unwrap();
    hook.wait().unwrap();
}

/// A stand-in `codex` that records its arguments, one per line, in
/// `codex-args`, and exits with `code`. A child process writes it: a write
/// handle this process held would be inherited by a child another test
/// thread forks, and running the stub meanwhile fails on Linux with "Text
/// file busy".
fn stub_codex(dir: &Path, code: i32) {
    use std::io::Write;
    let stubs = dir.join("stubs");
    std::fs::create_dir_all(&stubs).unwrap();
    let codex = stubs.join("codex");
    let args = dir.join("codex-args");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho 'queue: refused' >&2\nexit {code}\n",
        args.display()
    );
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "cat > \"$0\" && chmod 755 \"$0\""])
        .arg(&codex)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(writer.wait().unwrap().success());
}

const CODEX_PAYLOAD: &str = r#"{"session_id":"thr-1","hook_event_name":"Stop"}"#;

#[test]
fn a_codex_waiter_queues_the_continuation_on_the_session_it_ended() {
    let dir = tempdir().unwrap();
    project(dir.path());
    stub_codex(dir.path(), 0);
    send(dir.path());

    let mut hook = start_with(
        dir.path(),
        &stop_hook_command(Harness::Codex),
        CODEX_PAYLOAD,
    );
    wait_for_exit(&mut hook);
    let out = hook.wait_with_output().unwrap();

    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let args = std::fs::read_to_string(dir.path().join("codex-args")).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        ["queue", "--thread", "thr-1", "--message", CONTINUATION]
    );
    assert_eq!(kind(dir.path()), None);
}

#[test]
fn a_codex_waiter_that_cannot_queue_leaves_the_agent_unarmed() {
    let dir = tempdir().unwrap();
    project(dir.path());
    stub_codex(dir.path(), 1);
    send(dir.path());

    let mut hook = start_with(
        dir.path(),
        &stop_hook_command(Harness::Codex),
        CODEX_PAYLOAD,
    );
    wait_for_exit(&mut hook);
    let out = hook.wait_with_output().unwrap();

    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let waiting = runtime::read_waiting(dir.path(), "main", AGENT).unwrap();
    assert_eq!(waiting.kind, WaitingKind::HookEnded);
    assert!(waiting.describe().contains("queue: refused"), "{waiting:?}");
}
