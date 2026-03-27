/// Test utilities shared across modules.
use std::sync::atomic::{AtomicU32, Ordering};

static TMUX_SERVER_COUNTER: AtomicU32 = AtomicU32::new(0);

/// RAII guard for a tmux test server. Kills the server on drop,
/// even if the test panics.
pub struct TestServer(String);

impl TestServer {
    pub fn new() -> Self {
        let id = TMUX_SERVER_COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        Self(format!("pm-test-{pid}-{id}"))
    }

    /// Get the server name to pass to tmux functions as `Some(&str)`.
    pub fn name(&self) -> Option<&str> {
        Some(&self.0)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = crate::tmux::kill_server(Some(&self.0));
    }
}
