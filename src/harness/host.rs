//! What an agent's window runs when pm owns the agent's harness server
//! (codex's and opencode's hosts): the server in a process group of its
//! own, the TUI in the window's foreground, both supervised until the TUI
//! exits or this process is told to end.
//!
//! The terminal's signals then reach only the TUI and the host, and the
//! host ends the server's whole group, which also ends the hooks and
//! plugin waits the server ran. A server started in the background of a
//! shell instead outlives a killed window: the TUI holds the terminal's
//! foreground group, so the hangup never reaches it, and the server would
//! then take a wake as a turn nobody sees.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use crate::error::Result;

/// How many times in a row a server that died under a live TUI is started
/// again; one that stays up for [`STAYED_UP`] resets the count.
const RESTARTS: u32 = 5;
const STAYED_UP: Duration = Duration::from_secs(60);
pub(super) const TICK: Duration = Duration::from_millis(100);

/// A child in a process group of its own, whose group is ended when it is
/// dropped: SIGTERM, then SIGKILL for whatever is left a moment later.
pub(super) struct Group(pub(super) Child);

impl Group {
    pub(super) fn spawn(command: &mut Command) -> std::io::Result<Self> {
        command.process_group(0).spawn().map(Self)
    }

    pub(super) fn exited(&mut self) -> bool {
        !matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let group = -(self.0.id() as libc::pid_t);
        // SAFETY: signalling a process group this process created; a gone
        // group only makes kill fail.
        unsafe { libc::kill(group, libc::SIGTERM) };
        let _ = wait_within(&mut self.0, Duration::from_secs(2));
        // SAFETY: as above.
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

/// Run until `tui` exits or this process is told to end (SIGHUP as the
/// window closes, SIGTERM, SIGINT), starting `server` again with `start`
/// whenever it has `exited` under the live TUI, within a budget. The server
/// is ended on return. Returns the TUI's exit code.
pub(super) fn supervise<S>(
    mut tui: Child,
    server: S,
    exited: impl Fn(&mut S) -> bool,
    mut start: impl FnMut() -> Result<S>,
    log: &dyn Fn(&str),
) -> Result<i32> {
    let mut server = Some(server);
    let mut restarts = 0;
    let mut started = Instant::now();
    loop {
        match tui.try_wait() {
            Ok(Some(status)) => return Ok(status.code().unwrap_or(1)),
            Ok(None) => {}
            Err(e) => {
                let _ = tui.kill();
                return Err(e.into());
            }
        }
        let signal = SIGNAL.swap(0, Ordering::SeqCst);
        if signal != 0 {
            drop(server);
            // SAFETY: signalling the child this process started.
            unsafe { libc::kill(tui.id() as libc::pid_t, signal) };
            let _ = wait_within(&mut tui, Duration::from_secs(5));
            let _ = tui.kill();
            let _ = tui.wait();
            return Ok(128 + signal);
        }
        let down = server.as_mut().is_none_or(&exited);
        if !down && started.elapsed() > STAYED_UP {
            restarts = 0;
        }
        if down && restarts == RESTARTS {
            restarts += 1;
            log(&format!(
                "its server died {RESTARTS} times in a row: not starting it again"
            ));
        }
        if down && restarts < RESTARTS {
            restarts += 1;
            log("its server exited under a live TUI: starting it again");
            drop(server.take());
            server = start().inspect_err(|e| log(&e.to_string())).ok();
            started = Instant::now();
        }
        std::thread::sleep(TICK);
    }
}

static SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::SeqCst);
}

/// Record SIGHUP, SIGTERM and SIGINT for [`supervise`] instead of dying of
/// them.
pub(super) fn catch_signals() {
    for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler only stores to an atomic.
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }
}
