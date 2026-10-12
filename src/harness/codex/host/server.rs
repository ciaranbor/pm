//! The agent's `codex app-server` process: started in a process group of
//! its own, once any server the agent had before is gone, and ended with
//! its group.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{CodexHost, TICK};
use crate::error::{PmError, Result};
use crate::harness::codex::BINARY;

/// The server's pid, in its runtime dir.
const SERVER_PID: &str = "app-server.pid";
/// How long a server may take to listen.
const LISTEN_WITHIN: Duration = Duration::from_secs(30);

/// The agent's app-server, in a process group of its own.
pub(super) struct Server(Child);

impl Server {
    /// Start the server once the agent's previous one is gone: a second
    /// server refuses a socket path one still holds, and one that ends
    /// removes its socket.
    pub(super) fn start(host: &CodexHost, socket: &Path, log_file: &Path) -> Result<Self> {
        let pid_file = socket.with_file_name(SERVER_PID);
        end_previous(&pid_file, socket);
        let deadline = Instant::now() + LISTEN_WITHIN;
        let mut log = std::fs::File::create(log_file)?;
        loop {
            if std::fs::symlink_metadata(socket).is_ok() {
                std::fs::remove_file(socket)?;
            }
            let child = Command::new(BINARY)
                .args(host.server_args(socket))
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log.try_clone()?)
                .process_group(0)
                .spawn()?;
            std::fs::write(&pid_file, child.id().to_string())?;
            let mut server = Self(child);
            while !server.exited() {
                if std::fs::symlink_metadata(socket).is_ok() {
                    return Ok(server);
                }
                if Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(TICK);
            }
            if Instant::now() > deadline {
                return Err(PmError::Agent(format!(
                    "codex app-server did not listen at {} (its output: {})",
                    socket.display(),
                    log_file.display()
                )));
            }
            std::io::Write::write_all(&mut log, b"pm: starting it again\n")?;
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    pub(super) fn exited(&mut self) -> bool {
        !matches!(self.0.try_wait(), Ok(None))
    }
}

/// End the server `pid_file` names, if it is still the one listening at
/// `socket`: one whose host is ending as this one starts, or one a killed
/// host left behind.
fn end_previous(pid_file: &Path, socket: &Path) {
    let Some(pid) = std::fs::read_to_string(pid_file)
        .ok()
        .and_then(|text| text.trim().parse::<libc::pid_t>().ok())
        .filter(|pid| *pid > 0)
    else {
        return;
    };
    let listen = format!("--listen unix://{}", socket.display());
    let is_server = || {
        Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&listen))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut signal = libc::SIGTERM;
    while is_server() {
        if Instant::now() > deadline {
            signal = libc::SIGKILL;
        }
        // SAFETY: signalling the group of a process checked to be this
        // agent's app-server.
        unsafe { libc::kill(-pid, signal) };
        std::thread::sleep(Duration::from_millis(200));
        if signal == libc::SIGKILL && Instant::now() > deadline + Duration::from_secs(2) {
            return;
        }
    }
}

impl Drop for Server {
    /// End the server's process group: SIGTERM, then SIGKILL for whatever
    /// is left a moment later.
    fn drop(&mut self) {
        let group = -(self.0.id() as libc::pid_t);
        // SAFETY: signalling a process group this process created; a gone
        // group only makes kill fail.
        unsafe { libc::kill(group, libc::SIGTERM) };
        let _ = wait_within(&mut self.0, Duration::from_secs(2));
        // SAFETY: as above; this also ends the hooks the server ran.
        unsafe { libc::kill(group, libc::SIGKILL) };
        let _ = self.0.wait();
    }
}

pub(super) fn wait_within(child: &mut Child, limit: Duration) -> Result<bool> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(false)
}
