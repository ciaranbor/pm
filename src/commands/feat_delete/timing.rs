//! The cleanup timing log: per-step durations appended to
//! `.pm/cleanup.log`, to diagnose intermittent merge and cleanup hangs.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Accumulates timing entries and appends them to a log file on flush.
/// Used to diagnose intermittent merge/cleanup hangs — the log file
/// survives even when the tmux session that ran the command is killed.
pub struct TimingLog {
    path: PathBuf,
    buf: String,
}

impl TimingLog {
    /// Create a new timing log that will write to `<pm_dir>/cleanup.log`.
    pub fn new(pm_dir: &Path, label: &str, name: &str) -> Self {
        let path = pm_dir.join("cleanup.log");
        let mut buf = String::new();
        let _ = writeln!(
            buf,
            "\n--- {label}:{name} {} ---",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        );
        Self { path, buf }
    }

    /// Record a step's duration. Only records if elapsed > 100ms.
    pub fn record(&mut self, step: &str, elapsed: std::time::Duration) {
        if elapsed.as_millis() > 100 {
            let _ = writeln!(self.buf, "  {step}: {:.1}s", elapsed.as_secs_f64());
        }
    }

    /// Record the total duration. Only records if elapsed > 500ms.
    pub fn record_total(&mut self, elapsed: std::time::Duration) {
        if elapsed.as_millis() > 500 {
            let _ = writeln!(self.buf, "  TOTAL: {:.1}s", elapsed.as_secs_f64());
        }
    }

    /// Flush accumulated entries to the log file. No-op if nothing was recorded
    /// beyond the header.
    pub fn flush(&self) {
        // Header is always 1 line; if buf only has that, nothing interesting happened
        if self.buf.lines().count() <= 1 {
            return;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = f.write_all(self.buf.as_bytes());
        }
    }
}
