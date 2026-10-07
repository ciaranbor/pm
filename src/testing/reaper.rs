//! Killing the test servers of test binaries that died without cleaning up.

use super::pid_is_alive;

/// Kill the tmux server listening on `socket`, if any. `-S` rather than
/// `-L` so the reaper can act on a socket dir other than tmux's default.
fn kill_server_at(socket: &std::path::Path) {
    let _ = std::process::Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .arg("kill-server")
        .output();
}

/// Enumerate `pm-test-<pid>` sockets in `dir` and kill any whose pid no
/// longer refers to a live process. Ignores the current process's own
/// socket and malformed filenames.
pub(super) fn reap_dead_test_servers(dir: &std::path::Path) {
    let entries = match std::fs::read_dir(dir) {
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
        // `pm-test-<pid>`, or `pm-test-<pid>-<suffix>` for a test's own server.
        let pid: u32 = match pid_str.split('-').next().unwrap_or_default().parse() {
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
        // `kill-server` does NOT remove the socket when the server is
        // already dead, so it is unlinked explicitly.
        kill_server_at(&entry.path());
        let _ = std::fs::remove_file(entry.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tmux_server::HERMETIC_SHELL;
    use std::process::Command;

    /// A tmux server on a socket in a private dir, which no other test
    /// binary's reaper scans. Killed on drop, before its dir is removed:
    /// a server whose socket is deleted keeps running, unreachable.
    struct PrivateServer {
        socket: std::path::PathBuf,
    }

    impl PrivateServer {
        fn start(dir: &std::path::Path, name: &str) -> Self {
            let socket = dir.join(name);
            let out = Command::new("tmux")
                .arg("-S")
                .arg(&socket)
                .args([
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    "x",
                    "-c",
                    "/tmp",
                ])
                .arg(HERMETIC_SHELL)
                .output()
                .unwrap();
            assert!(out.status.success(), "start tmux on {socket:?}: {out:?}");
            Self { socket }
        }

        fn running(&self) -> bool {
            Command::new("tmux")
                .arg("-S")
                .arg(&self.socket)
                .arg("has-session")
                .output()
                .is_ok_and(|o| o.status.success())
        }
    }

    impl Drop for PrivateServer {
        fn drop(&mut self) {
            kill_server_at(&self.socket);
        }
    }

    /// A socket dir under `/tmp`: `$TMPDIR` on macOS is long enough to push
    /// a socket path past the 104-byte `sun_path` limit.
    fn private_socket_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("pm-reap-")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn dead_pid() -> u32 {
        let child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        let _ = child.wait_with_output();
        // Very rare flakes possible if the pid is reused immediately.
        assert!(!pid_is_alive(pid), "pid {pid} unexpectedly alive");
        pid
    }

    #[test]
    fn reaper_kills_dead_pid_server() {
        let dir = private_socket_dir();
        let leaked = PrivateServer::start(dir.path(), &format!("pm-test-{}", dead_pid()));

        reap_dead_test_servers(dir.path());

        assert!(!leaked.running(), "reaper left the dead-pid server running");
        assert!(!leaked.socket.exists(), "reaper left the dead-pid socket");
    }

    #[test]
    fn reaper_preserves_live_pid_server() {
        // Wrap the child in an RAII guard so we can't leak the sleep process
        // if an assertion panics mid-test.
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let dir = private_socket_dir();
        let guard = ChildGuard(Command::new("sleep").arg("30").spawn().unwrap());
        let live = PrivateServer::start(dir.path(), &format!("pm-test-{}", guard.0.id()));

        reap_dead_test_servers(dir.path());

        assert!(live.running(), "reaper killed a live-pid server");
    }

    #[test]
    fn reaper_ignores_unrelated_sockets() {
        let dir = private_socket_dir();
        // A file whose pid segment does not parse as an integer. Must be
        // untouched by the reaper (and the reaper must not panic).
        let bogus = dir.path().join("pm-test-notanumber");
        std::fs::File::create(&bogus).unwrap();

        reap_dead_test_servers(dir.path());

        assert!(bogus.exists(), "reaper removed an unparseable filename");
    }
}
