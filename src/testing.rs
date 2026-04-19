/// Test utilities shared across modules.
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

static TMUX_SERVER_COUNTER: AtomicU32 = AtomicU32::new(0);
static SHARED_SERVER_NAME: OnceLock<String> = OnceLock::new();

/// PID whose `pm-test-<pid>` server should be killed by the atexit handler.
/// Stored separately because `extern "C" fn` cannot capture state.
static ATEXIT_PID: AtomicU32 = AtomicU32::new(0);

/// Hard ceiling on concurrent live sessions in the shared test server.
/// Exceeding this indicates a leak — the test that trips it panics with a
/// recovery command instead of silently exhausting the system pty budget.
const MAX_TEST_SESSIONS: usize = 200;

/// System-wide pty ceiling. If the total number of allocated ptys on the
/// system reaches this threshold, tests abort before creating more. The
/// macOS hard limit is 511; this leaves headroom for the user's own
/// sessions and agents.
const MAX_SYSTEM_PTYS: usize = 300;

/// Count system-wide allocated ptys by reading `/dev/ttys*` entries.
/// Returns `None` if the count cannot be determined.
fn system_pty_count() -> Option<usize> {
    let entries = std::fs::read_dir("/dev").ok()?;
    let count = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("ttys"))
        })
        .count();
    Some(count)
}

/// Check the system-wide pty count and return an error message if it
/// exceeds the safety threshold.
fn enforce_system_pty_cap() -> Result<(), String> {
    if let Some(count) = system_pty_count() {
        if count >= MAX_SYSTEM_PTYS {
            return Err(format!(
                "system-wide pty count is {count} (threshold: {MAX_SYSTEM_PTYS}, macOS limit: 511). \
                 Aborting test to prevent pty exhaustion. \
                 Check for leaked tmux sessions: tmux list-sessions; \
                 kill test servers: for s in /tmp/tmux-$(id -u)/pm-test-*; do tmux -L $(basename \"$s\") kill-server; done"
            ));
        }
    }
    Ok(())
}

/// Directory tmux uses for its unix sockets. Honours `TMUX_TMPDIR` (matching
/// tmux itself) and falls back to `/tmp/tmux-<uid>`.
fn tmux_socket_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("TMUX_TMPDIR")
        && !dir.is_empty()
    {
        return std::path::PathBuf::from(dir);
    }
    // Safety: getuid is always safe to call.
    let uid = unsafe { libc::getuid() };
    std::path::PathBuf::from(format!("/tmp/tmux-{uid}"))
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

/// Enumerate `pm-test-<pid>` sockets in the tmux socket dir and kill any
/// whose pid no longer refers to a live process. Ignores the current
/// process's own socket and malformed filenames.
fn reap_dead_test_servers() {
    let dir = tmux_socket_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let self_pid = std::process::id();
    for entry in entries.flatten() {
        let fname = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let pid_str = match fname.strip_prefix("pm-test-") {
            Some(s) => s,
            None => continue,
        };
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        // Skip our own pid: `pid_is_alive(self_pid)` is always true, and
        // the stale-self-pid case (pid reuse across test binaries) is
        // handled in `shared_server_name()` at init time, before we create
        // our own server. Stripping it here also keeps the reaper safe to
        // call after init without risking killing our live server.
        if pid == self_pid {
            continue;
        }
        if pid_is_alive(pid) {
            continue;
        }
        // Ask tmux to kill the server first (in case one is still running
        // under this socket), then unlink the socket file. `kill-server`
        // does NOT remove the socket when the server is already dead, so we
        // must unlink it explicitly to avoid leaving stale files forever.
        let _ = crate::tmux::kill_server(Some(&fname));
        let _ = std::fs::remove_file(entry.path());
    }
}

/// Check the soft cap on live sessions. Returns `Err(message)` when the
/// caller should panic; the message is the exact recovery hint shown to
/// the user. Pure function so it can be unit-tested directly.
fn enforce_soft_cap(count: usize, pid: u32) -> Result<(), String> {
    if count > MAX_TEST_SESSIONS {
        Err(format!(
            "pty budget exceeded ({count} sessions in pm-test-{pid}). \
             This usually indicates leaked test sessions. \
             Recover with: tmux -L pm-test-{pid} kill-server"
        ))
    } else {
        Ok(())
    }
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

fn shared_server_name() -> &'static str {
    SHARED_SERVER_NAME.get_or_init(|| {
        // Reap any `pm-test-<pid>` servers left behind by dead test binaries
        // (SIGKILL, abort, or anything else that bypassed our atexit).
        reap_dead_test_servers();

        let pid = std::process::id();
        let name = format!("pm-test-{pid}");

        // Pid-reuse edge case: a previous test binary may have exited
        // leaving a `pm-test-<pid>` socket on disk, and this fresh process
        // happens to be assigned the same pid. Unlink any stale socket
        // under our own name before creating the real one, otherwise tmux
        // would try to connect to the old socket and fail.
        let _ = crate::tmux::kill_server(Some(&name));
        let _ = std::fs::remove_file(tmux_socket_dir().join(&name));

        // Create a keepalive session so the server stays alive for the entire
        // test run. Without this, the server shuts down each time a test cleans
        // up its sessions (costing ~2s to cold-start per subsequent test).
        let _ = crate::tmux::create_session(Some(&name), "keepalive", std::path::Path::new("/tmp"));

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
    prefix: String,
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

    /// Get the shared server name to pass to tmux functions as `Some(&str)`.
    pub fn name(&self) -> Option<&str> {
        Some(shared_server_name())
    }

    /// Return a name scoped to this test instance. Use this for project names
    /// and direct session names to avoid collisions with other parallel tests.
    pub fn scope(&self, name: &str) -> String {
        format!("{}-{}", self.prefix, name)
    }

    /// Create a project with init, returning `(project_path, projects_dir, project_name)`.
    pub fn setup_project(
        &self,
        dir: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let name = self.scope("myapp");
        let project_path = dir.join(&name);
        let projects_dir = dir.join("registry");
        crate::commands::init::init(&project_path, &projects_dir, None, self.name()).unwrap();
        (project_path, projects_dir, name)
    }

    /// Create a project and a feature, returning `(project_path, project_name)`.
    pub fn setup_project_with_feature(
        &self,
        dir: &std::path::Path,
        feature_name: &str,
    ) -> (std::path::PathBuf, String) {
        let (project_path, _, project_name) = self.setup_project(dir);
        crate::commands::feat_new::feat_new(
            &project_path,
            feature_name,
            None,
            None,
            None,
            false,
            None,
            self.name(),
        )
        .unwrap();
        (project_path, project_name)
    }

    /// Add a commit to a feature worktree.
    pub fn add_feature_commit(project_path: &std::path::Path, feature_name: &str) {
        let worktree = project_path.join(feature_name);
        std::fs::write(worktree.join("feature.txt"), "feature work").unwrap();
        crate::git::stage_file(&worktree, "feature.txt").unwrap();
        crate::git::commit(&worktree, "feature work").unwrap();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Wait up to ~1s for a filesystem path to exist (or not).
    fn wait_for(path: &std::path::Path, should_exist: bool) -> bool {
        for _ in 0..100 {
            if path.exists() == should_exist {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        path.exists() == should_exist
    }

    #[test]
    fn reaper_kills_dead_pid_server() {
        // Ensure shared server is initialised (and atexit registered) so we
        // don't accidentally test the init path here.
        let _server = TestServer::new();

        // Spawn a short-lived child, capture its pid, wait for it to exit.
        let child = Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        let _ = child.wait_with_output();

        // Sanity: that pid should not be alive now. (Very rare flakes possible
        // if pid gets reused immediately — acceptable.)
        assert!(!pid_is_alive(dead_pid), "pid {dead_pid} unexpectedly alive");

        // Create a "leaked" tmux server under pm-test-<dead_pid>.
        let leaked_name = format!("pm-test-{dead_pid}");
        crate::tmux::create_session(Some(&leaked_name), "x", std::path::Path::new("/tmp")).unwrap();
        let socket = tmux_socket_dir().join(&leaked_name);
        assert!(wait_for(&socket, true), "socket was never created");

        reap_dead_test_servers();

        assert!(
            wait_for(&socket, false),
            "reaper failed to kill dead-pid server at {socket:?}"
        );
    }

    #[test]
    fn reaper_preserves_live_pid_server() {
        let _server = TestServer::new();

        // Spawn a long-lived child so its pid is guaranteed alive during the
        // reap call. Wrap it in an RAII guard so we can't leak the sleep(1)
        // process if an assertion panics mid-test.
        struct ChildGuard(Option<std::process::Child>);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                if let Some(mut c) = self.0.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        }

        let guard = ChildGuard(Some(Command::new("sleep").arg("30").spawn().unwrap()));
        let live_pid = guard.0.as_ref().unwrap().id();

        let live_name = format!("pm-test-{live_pid}");
        crate::tmux::create_session(Some(&live_name), "x", std::path::Path::new("/tmp")).unwrap();
        let socket = tmux_socket_dir().join(&live_name);
        assert!(wait_for(&socket, true));

        reap_dead_test_servers();

        let preserved = socket.exists();

        // Clean up unconditionally before asserting, so a failure doesn't
        // leave a tmux server running.
        let _ = crate::tmux::kill_server(Some(&live_name));
        drop(guard);

        assert!(preserved, "reaper killed a live-pid server at {socket:?}");
    }

    #[test]
    fn reaper_ignores_unrelated_sockets() {
        let _server = TestServer::new();
        let dir = tmux_socket_dir();
        std::fs::create_dir_all(&dir).unwrap();

        // A file whose pid segment does not parse as an integer. Must be
        // untouched by the reaper (and the reaper must not panic).
        let bogus = dir.join("pm-test-notanumber");
        std::fs::File::create(&bogus).unwrap();

        reap_dead_test_servers();

        assert!(bogus.exists(), "reaper removed an unparseable filename");
        let _ = std::fs::remove_file(&bogus);
    }

    #[test]
    fn soft_cap_helper_allows_counts_at_or_below_max() {
        // Boundary: exactly MAX_TEST_SESSIONS must NOT trip the cap.
        assert!(enforce_soft_cap(MAX_TEST_SESSIONS, 123).is_ok());
        assert!(enforce_soft_cap(0, 123).is_ok());
        assert!(enforce_soft_cap(1, 123).is_ok());
    }

    #[test]
    fn soft_cap_helper_rejects_counts_above_max_with_recovery_hint() {
        let pid = 4242;
        let err = enforce_soft_cap(MAX_TEST_SESSIONS + 1, pid).expect_err("cap should trip");
        assert!(
            err.contains("pty budget exceeded"),
            "message missing header: {err}"
        );
        assert!(
            err.contains(&format!("tmux -L pm-test-{pid} kill-server")),
            "message missing recovery command: {err}"
        );
        assert!(
            err.contains(&format!("{} sessions", MAX_TEST_SESSIONS + 1)),
            "message missing count: {err}"
        );
    }

    #[test]
    fn soft_cap_panics_when_exceeded() {
        // End-to-end: the production path in TestServer::new() panics when
        // the helper returns Err. We can't realistically push the shared
        // server past 200 sessions inside a unit test, so exercise the
        // panic path indirectly by invoking the same code TestServer::new()
        // does and asserting it panics with the right message.
        let result = std::panic::catch_unwind(|| {
            if let Err(msg) = enforce_soft_cap(MAX_TEST_SESSIONS + 1, std::process::id()) {
                panic!("{msg}");
            }
        });
        let err = result.expect_err("soft cap did not panic");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(
            msg.contains("kill-server"),
            "panic message missing recovery hint: {msg}"
        );
    }
}
