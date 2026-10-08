//! Every test tmux session consumes a pty: the caps that abort a run before
//! leaked sessions exhaust the system's.

/// Hard ceiling on concurrent live sessions in the shared test server.
/// Exceeding this indicates a leak — the test that trips it panics with a
/// recovery command instead of silently exhausting the system pty budget.
pub(super) const MAX_TEST_SESSIONS: usize = 200;

/// Check the soft cap on live sessions. Returns `Err(message)` when the
/// caller should panic; the message is the exact recovery hint shown to
/// the user. Pure function so it can be unit-tested directly.
pub(super) fn enforce_soft_cap(count: usize, pid: u32) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::system_ptys::{check_system_ptys, system_ptys};

    #[test]
    fn the_system_pty_count_and_limit_are_readable() {
        let (limit, _) = system_ptys().expect("read the system's ptys");
        assert!(limit > 0);
    }

    #[test]
    fn the_system_cap_trips_at_its_share_of_the_limit() {
        assert!(check_system_ptys(511, 305).is_ok());
        assert!(check_system_ptys(511, 306).is_err());
        assert!(check_system_ptys(4096, 2000).is_ok());
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
