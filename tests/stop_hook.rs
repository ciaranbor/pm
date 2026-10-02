//! The installed Stop hook command, run the way a harness runs it, against
//! signals from the harness and from anyone else.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pm::commands::hooks_install::stop_hook_command;
use pm::state::paths;
use pm::state::runtime::{self, WaitingKind};
use tempfile::tempdir;

const AGENT: &str = "implementer";

fn project(dir: &Path) {
    std::fs::create_dir_all(dir.join(".pm")).unwrap();
    std::fs::create_dir_all(dir.join("main")).unwrap();
}

/// The installed command under `/bin/sh -c`, as Claude Code runs it, with
/// the built `pm` first on `PATH`. Returns once the hook is about to wait.
fn spawn_hook(dir: &Path) -> Child {
    let bin = assert_cmd::cargo::cargo_bin("pm");
    let path = format!(
        "{}:{}",
        bin.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let hook = Command::new("/bin/sh")
        .args(["-c", &stop_hook_command()])
        .env(
            "PM_TMUX_SERVER",
            format!("pm-test-{}-stop-none", std::process::id()),
        )
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("HOME", dir)
        .env("PATH", path)
        .env("PM_AGENT_NAME", AGENT)
        .current_dir(dir.join("main"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
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
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!(
            "Stop hook ended by SIGTERM from pid {}",
            std::process::id()
        )),
        "{stderr}"
    );
    let waiting = runtime::read_waiting(dir.path(), "main", AGENT).unwrap();
    assert_eq!(waiting.kind, WaitingKind::HookEnded);
}
