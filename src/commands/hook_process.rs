//! The process a blocking hook runs as: who ran it, and the signals it
//! catches (`Signals`). Shared by the hooks that wait — the Stop hook and
//! the dialog hook — which must end once the harness that ran them is gone,
//! since a hook blocked in its wait outlives a harness that dies without
//! killing it: codex never kills it, and Claude Code kills the hook's
//! process group on a clean exit but not when it is SIGKILLed.
//!
//! Two signals, either sufficient: our parent pid changes (the installed
//! command execs pm, so the harness is our parent and its death reparents
//! us), or the peer of our stdout closes (an install that predates the
//! `exec` leaves an intermediate `/bin/sh` as our parent, which is orphaned
//! instead, so the parent never changes; codex and the opencode plugin read
//! stdout through a pipe, Claude Code through a socketpair, and `poll`
//! reports a closed peer of either as `POLLHUP`/`POLLERR`). Neither fires
//! while the harness is alive, so a live agent's hook keeps blocking.

mod signals;

pub(crate) use signals::{Caught, Signals};

/// The harness process that ran this hook, as it was when the hook started.
pub(crate) struct Caller {
    parent: libc::pid_t,
}

impl Caller {
    pub(crate) fn current() -> Self {
        // SAFETY: getppid() takes no arguments and cannot fail.
        Self {
            parent: unsafe { libc::getppid() },
        }
    }

    /// Whether `caught` should end the wait. A SIGTERM must come from the
    /// harness: it is what `kill`, `pkill` and `killall` send by default, so
    /// a stray one must not end a hook the harness still waits on. SIGINT
    /// and SIGHUP are what a terminal sends, and macOS reports a terminal's
    /// signal as sent by whichever process wrote to it, so they end the
    /// wait from anyone.
    pub(crate) fn sent(&self, caught: &Caught) -> bool {
        caught.signal != libc::SIGTERM || caught.sender == self.parent
    }

    /// See the module docs for why both checks are needed.
    pub(crate) fn alive(&self) -> bool {
        // SAFETY: as above.
        let parent = unsafe { libc::getppid() };
        parent == self.parent && !peer_closed(libc::STDOUT_FILENO)
    }
}

/// Whether the reading end of `fd` — a pipe or socket — has been closed.
/// False for anything `poll` reports no hang-up on (a tty, a file,
/// `/dev/null`) and for a closed `fd`, so a hook run by hand keeps waiting.
fn peer_closed(fd: libc::c_int) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: one valid pollfd, count 1, zero timeout.
    let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
    ready > 0 && pfd.revents & (libc::POLLHUP | libc::POLLERR) != 0
}

/// Whether process `pid` exists.
pub(crate) fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    pid > 0
        && (unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_closed_tracks_the_reading_end() {
        let mut fds = [0; 2];
        // SAFETY: fds has room for the two descriptors pipe() writes.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let [read, write] = fds;
        assert!(!peer_closed(write));
        // SAFETY: closing descriptors this test owns.
        unsafe { libc::close(read) };
        assert!(peer_closed(write));
        unsafe { libc::close(write) };
    }

    #[test]
    fn a_reaped_child_is_not_alive() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(pid));
    }
}
