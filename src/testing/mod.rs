//! Test utilities shared across modules.
//!
//! Each test binary owns one `pm-test-<pid>` tmux server (hermetic: no tmux
//! config, `/bin/sh` windows, a `keepalive` session). Servers of dead pids
//! are reaped as the next run starts, and the current one is killed at exit.
//! Every session consumes a pty, so [`TestServer::new`] aborts the run once
//! the system-wide count reaches a budget below the macOS limit, and
//! `.cargo/config.toml` caps runs at 4 threads. A budget failure means leaked
//! sessions. To recover from a runaway run, kill its server with
//! `tmux -L pm-test-<pid> kill-server`, or every test server with
//! [`KILL_ALL_TEST_SERVERS`](pty_budget::KILL_ALL_TEST_SERVERS).
use std::sync::RwLock;

mod control_client;
mod fake_agents;
mod fake_harness;
mod home;
mod projects;
mod pty_budget;
mod reaper;
mod tmux_server;

pub use control_client::ControlClient;
pub(crate) use fake_harness::{HOLD_START, fake_claude, fake_harness_binary, window_path};
pub use fake_harness::{
    fake_opencode, fake_opencode_argv, fake_opencode_calls, fake_opencode_scripted,
    fake_opencode_sequence,
};
pub use home::test_home;
pub use tmux_server::{OwnServer, TestServer, server_socket_exists};

/// Process-wide lock guarding the global current working directory.
///
/// CWD is process-global, but `cargo test` runs tests in parallel within a
/// single process. Tests that mutate CWD via `std::env::set_current_dir`
/// take the WRITE lock; tests that merely *read* CWD take a READ lock. The
/// chief reader is `git clone`, which reads CWD at startup and aborts with
/// "this operation must be run in a work tree" if CWD has been moved into a
/// directory that isn't a work tree. This serialises the CWD mutator(s)
/// against the clone readers without flattening parallelism among the
/// readers themselves.
///
/// If you add a test that calls `std::env::set_current_dir`, take the write
/// lock here. If you add a test that runs `git clone` (or otherwise depends
/// on CWD), take the read lock.
pub static CWD_LOCK: RwLock<()> = RwLock::new(());

/// Serialises tests that write the test home's `.codex/config.toml` (codex
/// directory trust): the write is read-modify-write, so two concurrent
/// tests could each drop the other's entry.
pub static CODEX_CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A directory in `worktree` that neither git nor pm can delete, as a
/// locked file makes one; [`unlock`] lets the test's temp dir go.
pub fn lock_in(worktree: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let locked = worktree.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("file"), "x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    locked
}

pub fn unlock(locked: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(locked, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The directory name Claude Code keeps the sessions of `path` under.
pub fn claude_key(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Check whether a pid refers to a live process. Uses `kill(pid, 0)` which
/// returns 0 for live processes and sets errno to ESRCH for dead ones.
fn pid_is_alive(pid: u32) -> bool {
    // Safety: kill with sig=0 performs permission/existence check only.
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM means a live process we don't own — still alive. Only ESRCH
    // indicates the pid is truly gone.
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
