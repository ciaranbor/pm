//! `pm tmux watch`: the one process per tmux server that keeps pm's options
//! current, started by `pm tmux init`.
//!
//! tmux runs a status line's `#()` job once per attached client, so a
//! refresh driven from there would run once per client, each reading the
//! same previous `@pm_attention` and alerting on the same change. A watcher
//! instead holds the server's watch lock for its life, so another started
//! for the same server — a config reload re-running init — exits at once.
//! [`refresh`] also takes turns on the server's refresh lock, so a manual
//! refresh racing the watcher reads what the other wrote.
//!
//! pm pushes the changes it makes itself ([`tmux_push`](super::tmux_push)),
//! so the poll is the backstop for what pm cannot see happen: a harness
//! exiting, a session or window killed outside pm, a state file edited by
//! hand, a push that failed. Those are rare and none is urgent to the
//! second, so the default interval is 30 s rather than tmux's own 15 s
//! status interval: each tick reads every project's state and the whole
//! process table.
//!
//! A watcher exits when its server does or `@pm-auto-refresh` is `off`, and
//! executes its binary afresh when that is replaced, so an upgrade reaches
//! it. A watcher starting, afresh or after an upgrade, re-sets the formats
//! pm owns ([`tmux_init`]), so a format change reaches a running
//! server without a config reload.

use std::fs::{File, TryLockError};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::Result;
use crate::tmux::{self, options};

use super::tmux_init;
use super::tmux_refresh::{lock_file, refresh};

pub const AUTO_REFRESH: &str = "@pm-auto-refresh";
const INTERVAL: &str = "@pm-refresh-interval";
const DEFAULT_INTERVAL: u64 = 30;

/// Refresh every `@pm-refresh-interval` seconds until the server goes or
/// auto-refresh is turned off. Returns at once while another watcher has
/// the server.
pub fn watch(projects_dir: &Path, tmux_server: Option<&str>) -> Result<()> {
    let Some(socket) = tmux::socket_path(tmux_server)? else {
        return Ok(());
    };
    let Some(_lock) = take_watch(&socket)? else {
        return Ok(());
    };
    let binary = Binary::current();
    // Anything printed would land in a pane.
    let _ = tmux_init::formats(tmux_server);
    loop {
        let Some(settings) = options::read_global(tmux_server, &[AUTO_REFRESH, INTERVAL])? else {
            return Ok(());
        };
        if settings.get(AUTO_REFRESH) == "off" {
            return Ok(());
        }
        // Anything printed would land in a pane; the next tick retries.
        let _ = refresh(projects_dir, tmux_server);
        if let Some(binary) = binary.as_ref().filter(|b| b.replaced()) {
            return Err(binary.exec().into());
        }
        let seconds = settings
            .get(INTERVAL)
            .parse()
            .unwrap_or(DEFAULT_INTERVAL)
            .max(1);
        std::thread::sleep(Duration::from_secs(seconds));
    }
}

/// The server's watch lock, unless another watcher holds it. A push
/// ([`tmux_push`](super::tmux_push)) takes it for an instant to see whether
/// a watcher runs, so a watcher starting just then retries briefly.
fn take_watch(socket: &str) -> Result<Option<File>> {
    for _ in 0..5 {
        if let Some(lock) = try_lock(socket, "watch")? {
            return Ok(Some(lock));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(None)
}

pub(super) fn try_lock(socket: &str, kind: &str) -> Result<Option<File>> {
    let file = lock_file(socket, kind)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

/// The running executable, as it was when the watcher started.
struct Binary {
    path: PathBuf,
    modified: SystemTime,
}

impl Binary {
    fn current() -> Option<Self> {
        // A test binary rebuilt mid-run must not re-execute the suite.
        if cfg!(test) {
            return None;
        }
        let path = std::env::current_exe().ok()?;
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        Some(Self { path, modified })
    }

    /// Whether a different file is at the path now. A path with nothing
    /// there is mid-replacement, not replaced.
    fn replaced(&self) -> bool {
        std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .is_ok_and(|m| m != self.modified)
    }

    /// Replace this process with the binary at the path, run as this one
    /// was. Returns only on failure. The locks close with this process's
    /// files, so the new one takes them afresh.
    fn exec(&self) -> std::io::Error {
        std::process::Command::new(&self.path)
            .args(std::env::args_os().skip(1))
            .exec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tmux_init::{TREE_FORMAT, TREE_FORMAT_OPTION};
    use crate::commands::{feat_new, feat_status::feat_status, init};
    use crate::state::feature::Progress;
    use crate::testing::OwnServer;
    use crate::tmux::options::{Scope, set};
    use std::sync::mpsc;
    use tempfile::tempdir;

    fn project(server: &OwnServer, dir: &Path) -> (PathBuf, PathBuf) {
        let project = dir.join("app");
        let projects_dir = dir.join("registry");
        init::init(&project, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();
        (project, projects_dir)
    }

    /// Run `watch` on a thread; the receiver gets its result when it returns.
    fn spawn_watch(projects_dir: &Path, server: &OwnServer) -> mpsc::Receiver<Result<()>> {
        let (tx, rx) = mpsc::channel();
        let projects_dir = projects_dir.to_path_buf();
        let server = server.name().unwrap().to_string();
        std::thread::spawn(move || {
            let _ = tx.send(watch(&projects_dir, Some(&server)));
        });
        rx
    }

    fn published_count(server: &OwnServer) -> String {
        options::read_global(server.name(), &["@pm_count"])
            .unwrap()
            .unwrap()
            .get("@pm_count")
            .to_string()
    }

    #[test]
    fn one_watcher_per_server_refreshes_until_turned_off() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("watch");
        let (project, projects_dir) = project(&server, dir.path());
        options::run(server.name(), &[set(Scope::Global, INTERVAL, Some("1"))]).unwrap();

        let first = spawn_watch(&projects_dir, &server);
        let wait = Duration::from_secs(10);
        let until = |what: &str, done: &dyn Fn() -> bool| {
            let start = std::time::Instant::now();
            while !done() {
                assert!(start.elapsed() < wait, "{what}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        until("first refresh", &|| published_count(&server) == "0");

        let second = spawn_watch(&projects_dir, &server);
        second
            .recv_timeout(Duration::from_millis(500))
            .expect("a second watcher returns at once")
            .unwrap();

        feat_status(&project, "login", Progress::Blocked, Some("why?"), None).unwrap();
        until("a later tick", &|| published_count(&server) == "1");

        options::run(
            server.name(),
            &[set(Scope::Global, AUTO_REFRESH, Some("off"))],
        )
        .unwrap();
        first.recv_timeout(wait).expect("turned off").unwrap();
    }

    #[test]
    fn a_starting_watcher_brings_pms_formats_up_to_date() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("watch-formats");
        let (_, projects_dir) = project(&server, dir.path());
        options::run(
            server.name(),
            &[
                set(Scope::Global, TREE_FORMAT_OPTION, Some("stale")),
                set(Scope::Global, "window-status-format", Some("#I #W")),
                set(Scope::Global, AUTO_REFRESH, Some("off")),
            ],
        )
        .unwrap();

        watch(&projects_dir, server.name()).unwrap();
        watch(&projects_dir, server.name()).unwrap();

        assert_eq!(
            options::show(server.name(), TREE_FORMAT_OPTION).unwrap(),
            TREE_FORMAT
        );
        assert_eq!(
            options::show(server.name(), "window-status-format").unwrap(),
            "#I #{?@pm_agent_badge,#{@pm_agent_badge} ,}#W"
        );
    }

    #[test]
    fn a_watcher_ends_with_its_server() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("watch-end");
        let (_, projects_dir) = project(&server, dir.path());
        options::run(server.name(), &[set(Scope::Global, INTERVAL, Some("1"))]).unwrap();

        let watcher = spawn_watch(&projects_dir, &server);
        tmux::kill_server(server.name()).unwrap();

        watcher
            .recv_timeout(Duration::from_secs(10))
            .expect("server gone")
            .unwrap();
    }
}
