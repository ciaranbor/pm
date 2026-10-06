//! SIGTERM, SIGHUP and SIGINT, caught for the rest of the process so a
//! blocking hook's wait can tell who sent a signal and act on it at once. A
//! handler may only do async-signal-safe work, so it writes the signal and
//! its sender's pid to a pipe the wait polls; a write of at most `PIPE_BUF`
//! bytes is atomic, so concurrent records never interleave.

use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

/// A caught signal and the pid that sent it (`si_pid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caught {
    pub signal: libc::c_int,
    pub sender: libc::pid_t,
}

impl std::fmt::Display for Caught {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.signal {
            libc::SIGTERM => "SIGTERM",
            libc::SIGHUP => "SIGHUP",
            _ => "SIGINT",
        };
        write!(f, "{name} from pid {}", self.sender)
    }
}

const RECORD: usize = 8;

pub(crate) struct Signals {
    read: libc::c_int,
    write: libc::c_int,
}

impl Drop for Signals {
    fn drop(&mut self) {
        Self::handle(None);
        SIGNAL_PIPE.store(-1, Ordering::SeqCst);
        // SAFETY: closing the pipe this value opened.
        unsafe {
            libc::close(self.read);
            libc::close(self.write);
        }
    }
}

static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(signal: libc::c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    let fd = SIGNAL_PIPE.load(Ordering::SeqCst);
    if fd < 0 {
        return;
    }
    // SAFETY: the kernel passes a valid siginfo_t to an SA_SIGINFO handler.
    let sender = if info.is_null() {
        0
    } else {
        // libc exposes the field through an accessor on Linux.
        #[cfg(target_os = "linux")]
        let pid = unsafe { (*info).si_pid() };
        #[cfg(not(target_os = "linux"))]
        let pid = unsafe { (*info).si_pid };
        pid
    };
    let mut record = [0u8; RECORD];
    record[..4].copy_from_slice(&signal.to_ne_bytes());
    record[4..].copy_from_slice(&sender.to_ne_bytes());
    // SAFETY: write(2) is async-signal-safe; the buffer lives on this stack.
    unsafe { libc::write(fd, record.as_ptr().cast(), RECORD) };
}

impl Signals {
    const CAUGHT: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

    /// `None` when the handlers can't be installed; the default actions
    /// then stay.
    pub fn install() -> Option<Self> {
        let mut fds = [0; 2];
        // SAFETY: fds has room for the two descriptors pipe() writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        let [read, write] = fds;
        // SAFETY: setting flags on descriptors this process just opened.
        unsafe {
            libc::fcntl(read, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(write, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(read, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(write, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        SIGNAL_PIPE.store(write, Ordering::SeqCst);
        Self::handle(Some(on_signal));
        Some(Self { read, write })
    }

    /// Install `handler` for every caught signal, or the default action.
    fn handle(
        handler: Option<extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void)>,
    ) {
        for signal in Self::CAUGHT {
            // SAFETY: a zeroed sigaction with either the default action or
            // an `extern "C"` handler that only touches an atomic and write(2).
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                match handler {
                    Some(handler) => {
                        action.sa_sigaction = handler as *const () as libc::sighandler_t;
                        action.sa_flags = libc::SA_SIGINFO;
                    }
                    None => action.sa_sigaction = libc::SIG_DFL,
                }
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
    }

    /// Sleep up to `timeout`, or until a signal is caught. Every signal
    /// caught since the last call, oldest first.
    pub fn pause(&self, timeout: Duration) -> Vec<Caught> {
        let mut pfd = libc::pollfd {
            fd: self.read,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: one valid pollfd, count 1.
        unsafe { libc::poll(&mut pfd, 1, ms) };
        let mut caught = Vec::new();
        let mut record = [0u8; RECORD];
        // SAFETY: reading whole records into a buffer of that size from a
        // non-blocking pipe this value owns.
        while unsafe { libc::read(self.read, record.as_mut_ptr().cast(), RECORD) }
            == RECORD as isize
        {
            let [s0, s1, s2, s3, p0, p1, p2, p3] = record;
            caught.push(Caught {
                signal: libc::c_int::from_ne_bytes([s0, s1, s2, s3]),
                sender: libc::pid_t::from_ne_bytes([p0, p1, p2, p3]),
            });
        }
        caught
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Handlers are process-wide, and another test's code may install its
    /// own for SIGHUP while this one waits on it, letting the signal kill
    /// the test binary; so the handlers live in a forked child, and the
    /// signal comes from its parent. SIGHUP is blocked across the fork so
    /// one sent before the child's handler is up waits for it.
    #[test]
    fn a_caught_signal_ends_the_pause_at_once_and_names_its_sender() {
        // SAFETY: a zeroed sigset_t is initialised by sigemptyset; the mask
        // changes only this thread's, and is restored after the fork. The
        // child makes async-signal-safe calls but for `pause`'s allocation,
        // and malloc is fork-safe on macOS and glibc.
        let pid = unsafe {
            let mut hup: libc::sigset_t = std::mem::zeroed();
            let mut old: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut hup);
            libc::sigaddset(&mut hup, libc::SIGHUP);
            libc::pthread_sigmask(libc::SIG_BLOCK, &hup, &mut old);
            let pid = libc::fork();
            if pid == 0 {
                libc::_exit(i32::from(!caught_from_parent(&hup)));
            }
            libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
            pid
        };
        assert!(pid > 0, "fork failed");
        std::thread::sleep(Duration::from_millis(50));
        let mut status = 0;
        // SAFETY: signalling and then waiting on the child just forked.
        unsafe {
            libc::kill(pid, libc::SIGHUP);
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
        }
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "status {status}"
        );
        let caught = Caught {
            signal: libc::SIGHUP,
            sender: 42,
        };
        assert_eq!(caught.to_string(), "SIGHUP from pid 42");
    }

    /// In the forked child: whether a pause ends promptly with the one
    /// SIGHUP its parent sends.
    fn caught_from_parent(hup: &libc::sigset_t) -> bool {
        let Some(signals) = Signals::install() else {
            return false;
        };
        // SAFETY: unblocking the SIGHUP the parent blocked for the fork.
        unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, hup, std::ptr::null_mut()) };
        let start = Instant::now();
        let caught = signals.pause(Duration::from_secs(30));
        // SAFETY: getppid() takes no arguments and cannot fail.
        let parent = unsafe { libc::getppid() };
        start.elapsed() < Duration::from_secs(5)
            && caught
                == [Caught {
                    signal: libc::SIGHUP,
                    sender: parent,
                }]
    }
}
