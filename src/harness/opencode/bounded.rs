//! Every command pm runs opencode for returns within a limit. A hung call
//! would otherwise hang the pm command that made it — a register or adopt
//! whose own work is done — with nothing printed.
//!
//! The call runs in a process group of its own, killed whole when it ends
//! or at the limit, so nothing it started — a standalone server — outlives
//! it. Being out of the terminal's foreground group, it would not see a
//! Ctrl-C that kills pm, so pm's SIGINT, SIGTERM and SIGHUP kill every
//! running call's group before taking their default action; a server pm
//! keeps up across calls is guarded the same way. A call's output is read
//! on threads never joined past the limit, since such a process can hold
//! the pipes open after the call itself has exited.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Once, mpsc};
use std::time::{Duration, Instant};

use crate::error::{PmError, Result};

/// The limit for a call that answers from opencode's own store.
pub(super) const CALL: Duration = Duration::from_secs(60);

/// The limit for a call that writes or reads a whole transcript.
pub(super) const TRANSFER: Duration = Duration::from_secs(300);

const POLL: Duration = Duration::from_millis(20);

/// Why a call gave no output.
#[derive(Debug)]
pub(super) enum Failure {
    Unrunnable {
        program: String,
        error: std::io::Error,
    },
    TimedOut {
        limit: Duration,
    },
}

impl Failure {
    /// The failure as said of the call `what`.
    pub(super) fn describe(&self, what: &str) -> String {
        match self {
            Failure::Unrunnable { program, error } => {
                format!("could not run `{program}` for {what}: {error}")
            }
            Failure::TimedOut { limit } => {
                format!("opencode {what} did not answer within {limit:?}; pm stopped it")
            }
        }
    }
}

/// Run `command` and collect its output, killing it once `limit` has passed.
pub(super) fn run(command: &mut Command, limit: Duration) -> std::result::Result<Output, Failure> {
    let started = Instant::now();
    let program = command.get_program().to_string_lossy().into_owned();
    let unrunnable = |error| Failure::Unrunnable {
        program: program.clone(),
        error,
    };
    let mut call = match Guarded::spawn(command.stdout(Stdio::piped()).stderr(Stdio::piped())) {
        Ok(call) => call,
        Err(error) => return Err(unrunnable(error)),
    };
    let stdout = read_to_end(call.child.stdout.take());
    let stderr = read_to_end(call.child.stderr.take());
    let timed_out = Failure::TimedOut { limit };

    let status: ExitStatus = loop {
        match call.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => return Err(unrunnable(error)),
        }
        if started.elapsed() >= limit {
            return Err(timed_out);
        }
        std::thread::sleep(POLL);
    };
    let remaining = || limit.saturating_sub(started.elapsed());
    let (Ok(stdout), Ok(stderr)) = (
        stdout.recv_timeout(remaining()),
        stderr.recv_timeout(remaining()),
    ) else {
        return Err(timed_out);
    };
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// [`run`] for the opencode call `what`, its failure as pm's error.
pub(super) fn output(command: &mut Command, what: &str, limit: Duration) -> Result<Output> {
    run(command, limit).map_err(|failure| PmError::Agent(failure.describe(what)))
}

/// A long-running opencode process pm talks to across several calls: in a
/// process group of its own, killed whole when dropped and, like a call's,
/// by pm's fatal signals.
pub(super) struct Guarded {
    pub(super) child: Child,
    _running: Running,
}

impl Guarded {
    pub(super) fn spawn(command: &mut Command) -> std::io::Result<Self> {
        install_signal_handlers();
        let child = command.process_group(0).spawn()?;
        let _running = Running::register(child.id() as libc::pid_t);
        Ok(Self { child, _running })
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        kill_group(self.child.id());
        let _ = self.child.wait();
    }
}

/// The process groups of the calls running now, 0 for a free slot: all a
/// signal handler may read. A call that finds no free slot runs unguarded.
static RUNNING: [AtomicI32; 16] = [const { AtomicI32::new(0) }; 16];

/// A call's group in [`RUNNING`] for as long as this lives.
struct Running(Option<&'static AtomicI32>);

impl Running {
    fn register(group: libc::pid_t) -> Self {
        Self(RUNNING.iter().find(|slot| {
            slot.compare_exchange(0, group, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        }))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(slot) = self.0 {
            slot.store(0, Ordering::SeqCst);
        }
    }
}

const FATAL_SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

fn install_signal_handlers() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        for signal in FATAL_SIGNALS {
            // SAFETY: the handler makes only async-signal-safe calls.
            unsafe {
                libc::signal(
                    signal,
                    on_fatal_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
                );
            }
        }
    });
}

extern "C" fn on_fatal_signal(signal: libc::c_int) {
    for slot in &RUNNING {
        let group = slot.load(Ordering::SeqCst);
        if group != 0 {
            // SAFETY: kill(2) is async-signal-safe.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
    // SAFETY: signal(2) and raise(3) are async-signal-safe; the default
    // action then ends pm as the signal would have.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// Kill every process of the group `leader` started.
fn kill_group(leader: u32) {
    // SAFETY: kill(2) takes any pid; a negative one names a process group.
    unsafe {
        libc::kill(-(leader as libc::pid_t), libc::SIGKILL);
    }
}

fn read_to_end(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, received) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut out);
        }
        let _ = sender.send(out);
    });
    received
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]).stdin(Stdio::null());
        command
    }

    #[test]
    fn a_call_that_hangs_is_stopped_at_the_limit_and_named() {
        let limit = Duration::from_millis(300);
        let asked = Instant::now();
        let err = output(&mut sh("sleep 30"), "session.list", limit)
            .unwrap_err()
            .to_string();
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "{:?}",
            asked.elapsed()
        );
        assert!(
            err.contains("opencode session.list did not answer within"),
            "{err}"
        );
    }

    #[test]
    fn a_call_whose_child_keeps_the_pipe_open_does_not_hang_the_caller() {
        let asked = Instant::now();
        let err = output(
            &mut sh("sleep 30 & echo answered"),
            "session.get",
            Duration::from_millis(300),
        )
        .unwrap_err()
        .to_string();
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "{:?}",
            asked.elapsed()
        );
        assert!(err.contains("opencode session.get did not answer"), "{err}");
    }

    /// Whether the process whose pid `file` holds is still running, given a
    /// moment to be reaped after a kill.
    fn still_running(file: &std::path::Path) -> bool {
        let pid: libc::pid_t = std::fs::read_to_string(file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..100 {
            // SAFETY: signal 0 only probes for the process.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }

    #[test]
    fn nothing_a_call_started_outlives_its_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pidfile.display());
        output(&mut sh(&script), "session.list", Duration::from_millis(300)).unwrap_err();
        assert!(!still_running(&pidfile));
    }

    #[test]
    fn a_call_that_answers_in_time_returns_both_streams_and_its_status() {
        let out = output(&mut sh("echo out; echo err >&2; exit 3"), "x", CALL).unwrap();
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
        assert_eq!(out.status.code(), Some(3));
    }
}
