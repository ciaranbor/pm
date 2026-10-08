//! The system-wide pty count against the system's limit: the cap that
//! aborts a test run before leaked sessions exhaust the system's ptys.
//! Shared with the sandbox tests (`tests/sandbox`), so it holds no tests.

/// Shell command that kills every test server and unlinks its socket.
pub const KILL_ALL_TEST_SERVERS: &str = r#"for s in /tmp/tmux-$(id -u)/pm-test-*; do tmux -L $(basename "$s") kill-server; rm -f "$s"; done"#;

/// The system's pty limit and how many ptys are allocated now, or `None`
/// where either cannot be read.
#[cfg(target_os = "macos")]
pub(super) fn system_ptys() -> Option<(usize, usize)> {
    let mut limit: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated and `limit`/`size` describe a
    // c_int buffer the call writes at most `size` bytes into.
    let read = unsafe {
        libc::sysctlbyname(
            c"kern.tty.ptmx_max".as_ptr(),
            (&raw mut limit).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    let limit = usize::try_from(limit).ok().filter(|_| read == 0)?;
    let count = std::fs::read_dir("/dev")
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("ttys"))
        })
        .count();
    Some((limit, count))
}

/// The system's pty limit and how many ptys are allocated now, or `None`
/// where either cannot be read.
#[cfg(not(target_os = "macos"))]
pub(super) fn system_ptys() -> Option<(usize, usize)> {
    let limit = std::fs::read_to_string("/proc/sys/kernel/pty/max")
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let count = std::fs::read_to_string("/proc/sys/kernel/pty/nr")
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((limit, count))
}

/// The share of the system's ptys a test run may take, leaving the rest to
/// the user's own sessions and agents.
fn budget(limit: usize) -> usize {
    limit * 3 / 5
}

/// An error message once the system-wide pty count reaches the budget for
/// the system's limit (on macOS, 306 of the default 511).
pub fn enforce_system_pty_cap() -> Result<(), String> {
    match system_ptys() {
        Some((limit, count)) => check_system_ptys(limit, count),
        None => Ok(()),
    }
}

pub(super) fn check_system_ptys(limit: usize, count: usize) -> Result<(), String> {
    let budget = budget(limit);
    if count >= budget {
        return Err(format!(
            "system-wide pty count is {count} (threshold: {budget}, system limit: {limit}). \
                 Aborting test to prevent pty exhaustion. \
                 Check for leaked tmux sessions: tmux list-sessions; \
                 kill test servers: {KILL_ALL_TEST_SERVERS}"
        ));
    }
    Ok(())
}
