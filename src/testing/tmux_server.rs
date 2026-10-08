//! The tmux servers tests run on: one shared hermetic server per test binary,
//! and a test's own when it attaches a client.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use super::fake_harness::window_path;
use super::pty_budget::enforce_soft_cap;
use super::reaper::reap_dead_test_servers;
use super::system_ptys::enforce_system_pty_cap;

static TMUX_SERVER_COUNTER: AtomicU32 = AtomicU32::new(0);

static SHARED_SERVER_NAME: OnceLock<String> = OnceLock::new();

/// PID whose `pm-test-<pid>` server should be killed by the atexit handler.
/// Stored separately because `extern "C" fn` cannot capture state.
static ATEXIT_PID: AtomicU32 = AtomicU32::new(0);

/// Directory tmux uses for its unix sockets: `tmux-<uid>` under
/// `TMUX_TMPDIR` (matching tmux itself), or under `/tmp`.
pub(super) fn tmux_socket_dir() -> std::path::PathBuf {
    let base = std::env::var("TMUX_TMPDIR")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/tmp".to_string());
    // Safety: getuid is always safe to call.
    let uid = unsafe { libc::getuid() };
    std::path::PathBuf::from(base).join(format!("tmux-{uid}"))
}

/// atexit handler: kill the shared test server owned by this process.
/// Registered once via `libc::atexit` after the server is created. Runs on
/// normal exit and after panic unwind; does NOT run on SIGKILL or abort()
/// — those are cleaned up by the startup reaper on the next run.
///
/// This runs after `main` returns while the C runtime and libstd are still
/// usable, so spawning a subprocess via `Command` is fine. It is NOT a
/// signal handler, so async-signal-safety rules do not apply.
extern "C" fn atexit_kill_shared_server() {
    let pid = ATEXIT_PID.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    let name = format!("pm-test-{pid}");
    let _ = crate::tmux::kill_server(Some(&name));
}

/// The shell every window of the shared test server runs.
pub(super) const HERMETIC_SHELL: &str = "/bin/sh";

/// Start a tmux server on socket `name` for tests to type commands into.
///
/// Under the developer's tmux config and interactive shell, a window's rc
/// files can take over 10s to load under a loaded suite, and can drop keys
/// typed before the prompt; so the server reads no config and every window
/// runs `sh` with no startup files. tmux starts a window as
/// `$default-shell -c <default-command>`, and takes `default-shell` from
/// `SHELL`, so both are set; with `ENV` unset the interactive `sh` reads
/// nothing. The command `exec`s the shell: dash (Debian's `/bin/sh`) would
/// otherwise stay as its parent, so the pane's own process would not be the
/// shell whose prompt [`running_agents`](crate::commands::running_agents)
/// reads. Its windows find the [`fake_harness_path`] stand-ins first
/// ([`window_path`]). The keepalive session keeps the server up: without it the
/// server shuts down each time a test cleans up its sessions.
fn start_hermetic_server(name: &str) -> bool {
    // A socket left under `name` by a dead run whose pid ours reuses would
    // stop tmux from starting a server on it.
    let _ = crate::tmux::kill_server(Some(name));
    let _ = std::fs::remove_file(tmux_socket_dir().join(name));
    std::process::Command::new("tmux")
        .env("SHELL", HERMETIC_SHELL)
        .env("PATH", window_path())
        .env_remove("ENV")
        .args(["-L", name, "-f", "/dev/null"])
        .args([
            "new-session",
            "-d",
            "-s",
            "keepalive",
            "-c",
            "/tmp",
            HERMETIC_SHELL,
        ])
        .args([";", "set-option", "-g", "default-shell", HERMETIC_SHELL])
        .args([
            ";",
            "set-option",
            "-g",
            "default-command",
            &format!("exec {HERMETIC_SHELL}"),
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A tmux server of one test's own, `pm-test-<pid>-<suffix>`, killed on
/// drop (and reaped with the shared server's if the run dies). For a test
/// that attaches a client: on the shared server it would be the client
/// other tests' `switch-client` and `#{client_session}` find.
pub struct OwnServer(String);

impl OwnServer {
    pub fn start(suffix: &str) -> Self {
        if let Err(msg) = enforce_system_pty_cap() {
            panic!("{msg}");
        }
        let name = format!("pm-test-{}-{suffix}", std::process::id());
        assert!(start_hermetic_server(&name), "start tmux server {name}");
        Self(name)
    }

    pub fn name(&self) -> Option<&str> {
        Some(&self.0)
    }

    pub fn tmux_stdout(&self, args: &[&str]) -> String {
        tmux_stdout(self.name(), args)
    }
}

/// `tmux args` on `server`, which must succeed, and its trimmed stdout.
fn tmux_stdout(server: Option<&str>, args: &[&str]) -> String {
    let output = std::process::Command::new("tmux")
        .args(["-L", server.unwrap()])
        .args(args)
        .output()
        .expect("run tmux");
    assert!(output.status.success(), "tmux {args:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

impl Drop for OwnServer {
    fn drop(&mut self) {
        let _ = crate::tmux::kill_server(self.name());
    }
}

/// Whether a server listens, or once listened, on socket `name`.
pub fn server_socket_exists(name: &str) -> bool {
    tmux_socket_dir().join(name).exists()
}

fn shared_server_name() -> &'static str {
    SHARED_SERVER_NAME.get_or_init(|| {
        // Reap any `pm-test-<pid>` servers left behind by dead test binaries
        // (SIGKILL, abort, or anything else that bypassed our atexit).
        reap_dead_test_servers(&tmux_socket_dir());

        let pid = std::process::id();
        let name = format!("pm-test-{pid}");

        start_hermetic_server(&name);

        // Register the atexit cleanup exactly once. Store the pid first
        // because the extern "C" fn cannot capture.
        ATEXIT_PID.store(pid, Ordering::SeqCst);
        // Safety: `libc::atexit` is always safe to call. The handler runs
        // after `main` returns while libstd is still usable (it is not a
        // signal handler), so invoking `Command` from inside it is fine.
        unsafe {
            libc::atexit(atexit_kill_shared_server);
        }
        name
    })
}

/// RAII guard for a shared tmux test server. All tests share a single tmux
/// server process (one per cargo-test binary) to minimise pty usage. Each
/// `TestServer` instance gets a unique prefix so session names don't collide
/// across parallel tests. On drop, only sessions belonging to this instance
/// are killed — the shared server stays alive for other tests.
pub struct TestServer {
    pub(super) prefix: String,
}

impl Default for TestServer {
    fn default() -> Self {
        Self::new()
    }
}

impl TestServer {
    pub fn new() -> Self {
        // System-wide pty check: abort before creating any new sessions
        // if we're approaching the macOS pty limit.
        if let Err(msg) = enforce_system_pty_cap() {
            panic!("{msg}");
        }

        let id = TMUX_SERVER_COUNTER.fetch_add(1, Ordering::SeqCst);
        let server = Self {
            prefix: format!("t{id}"),
        };

        // Soft cap: if the shared server is holding an unreasonable number
        // of sessions we've almost certainly leaked. Fail loudly with a
        // recovery command rather than cascading into system-wide pty
        // exhaustion.
        let count = crate::tmux::list_sessions(server.name())
            .map(|s| s.len())
            .unwrap_or(0);
        if let Err(msg) = enforce_soft_cap(count, std::process::id()) {
            panic!("{msg}");
        }

        server
    }

    /// The registry dir `setup_project` wrote for a project at `project_path`:
    /// a sibling `registry/` directory.
    pub fn registry_dir(project_path: &std::path::Path) -> std::path::PathBuf {
        project_path.parent().unwrap().join("registry")
    }

    /// Get the shared server name to pass to tmux functions as `Some(&str)`.
    pub fn name(&self) -> Option<&str> {
        Some(shared_server_name())
    }

    /// Return a name scoped to this test instance. Use this for project names
    /// and direct session names to avoid collisions with other parallel tests.
    pub fn scope(&self, name: &str) -> String {
        format!("{}-{}", self.prefix, name)
    }

    /// Set a variable every new window of `session` inherits, as when the
    /// session was created from a shell that had it.
    pub fn set_session_env(&self, session: &str, key: &str, value: &str) {
        let mut command = std::process::Command::new("tmux");
        if let Some(name) = self.name() {
            command.args(["-L", name]);
        }
        let status = command
            .args(["set-environment", "-t", session, key, value])
            .status()
            .expect("run tmux");
        assert!(status.success(), "tmux set-environment {key}");
    }

    /// The id (`%N`) of the pane `target` names; a window names its active
    /// pane.
    pub fn pane_id(&self, target: &str) -> String {
        self.tmux_stdout(&["display-message", "-p", "-t", target, "#{pane_id}"])
    }

    /// Split a new pane, running a shell, in front of `window`'s first pane,
    /// so it becomes pane 0 and the active pane, as a user splitting an
    /// agent's window might. Returns its id.
    pub fn split_before(&self, window: &str) -> String {
        let first = format!("{window}.0");
        self.tmux_stdout(&["split-window", "-b", "-P", "-F", "#{pane_id}", "-t", &first])
    }

    pub fn tmux_stdout(&self, args: &[&str]) -> String {
        tmux_stdout(self.name(), args)
    }

    /// Poll a window's agent pane's scrollback until `needle` appears (the shell echoes
    /// typed commands, so this observes what a spawn actually launched).
    /// Panics with the captured text on timeout.
    pub fn wait_for_pane_text(&self, target: &str, needle: &str) {
        let mut last = String::new();
        for _ in 0..500 {
            if let Ok(text) = crate::tmux::capture_pane(self.name(), target) {
                if text.contains(needle) {
                    return;
                }
                last = text;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "wait_for_pane_text: '{needle}' never appeared in window '{target}'; last capture:\n{last}"
        );
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Kill only sessions whose name starts with our prefix
        let prefix = format!("{}-", self.prefix);
        if let Ok(sessions) = crate::tmux::list_sessions(self.name()) {
            for s in sessions {
                if s.starts_with(&prefix) {
                    let _ = crate::tmux::kill_session(self.name(), &s);
                }
            }
        }
    }
}
