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
//! A watcher exits when its server does, taking the server's lock files
//! ([`tmux_lock`]) with it, or when `@pm-auto-refresh` is `off`. It
//! executes its binary afresh when that is replaced, so an upgrade reaches
//! it, and executes another when the `pm` the server would start changes
//! (`@pm-bin`, or the server's `PATH`), so it runs the one a watcher started
//! now would. A watcher starting, afresh or after an upgrade, re-sets the
//! formats pm owns ([`tmux_init`]), so a format change reaches a running
//! server without a config reload.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::Result;
use crate::tmux::{self, options};

use super::reexec::{self, Binary};
use super::tmux_init;
use super::tmux_lock::{self, Lock};
use super::tmux_refresh::refresh;

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
    let Some(lock) = take_watch(&socket)? else {
        return Ok(());
    };
    let binary = Binary::current();
    let started_as = server_pm(tmux_server);
    // Anything printed would land in a pane.
    let _ = tmux_init::formats(tmux_server);
    let _ = tmux_lock::prune();
    loop {
        let Some(settings) = options::read_global(tmux_server, &[AUTO_REFRESH, INTERVAL])? else {
            return tmux_lock::remove(&socket, lock);
        };
        if settings.get(AUTO_REFRESH) == "off" {
            return Ok(());
        }
        // Anything printed would land in a pane; the next tick retries.
        let _ = refresh(projects_dir, tmux_server);
        if let Some(binary) = binary.as_ref() {
            if binary.replaced() {
                return Err(binary.exec().into());
            }
            let now = server_pm(tmux_server);
            if let Some(path) = now.as_ref().filter(|_| now != started_as) {
                return Err(reexec::exec(path).into());
            }
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
fn take_watch(socket: &str) -> Result<Option<Lock>> {
    for _ in 0..5 {
        if let Some(lock) = tmux_lock::try_lock(socket, "watch")? {
            return Ok(Some(lock));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(None)
}

/// The `pm` the server would start as its watcher: `@pm-bin`, else `pm`
/// on the server's `PATH`, symlinks followed. `None` when it can't be read
/// or found.
fn server_pm(tmux_server: Option<&str>) -> Option<PathBuf> {
    let settings = options::read_global(tmux_server, &[tmux_init::BIN]).ok()??;
    let bin = match settings.get(tmux_init::BIN) {
        "" => "pm",
        bin => bin,
    };
    let path = tmux::global_environment(tmux_server, "PATH").ok()?;
    crate::fs_utils::resolve_binary(bin, path.as_deref().map(std::ffi::OsStr::new))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tmux_init::{TREE_FORMAT, TREE_FORMAT_OPTION};
    use crate::commands::{feat_new, feat_status::feat_status, init};
    use crate::state::feature::Progress;
    use crate::state::paths;
    use crate::testing::OwnServer;
    use crate::tmux::options::{Scope, set};
    use std::path::PathBuf;
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
    fn the_pm_a_watcher_would_start_follows_the_servers_path_and_pm_bin() {
        let server = OwnServer::start("watch-pm");
        let dir = tempdir().unwrap();
        let pm_in = |name: &str| {
            let bin = dir.path().join(name);
            std::fs::create_dir_all(&bin).unwrap();
            let pm = bin.join("pm");
            crate::testing::write_executable(&pm, "#!/bin/sh\n");
            (bin, pm.canonicalize().unwrap())
        };
        let (first_dir, first) = pm_in("first");
        let (second_dir, second) = pm_in("second");
        let path = |dirs: &[&Path]| {
            let joined = std::env::join_paths(dirs).unwrap();
            let joined = joined.to_str().unwrap().to_string();
            options::run(
                server.name(),
                &[vec![
                    "set-environment".into(),
                    "-g".into(),
                    "PATH".into(),
                    joined,
                ]],
            )
            .unwrap();
        };

        path(&[&first_dir, &second_dir]);
        assert_eq!(server_pm(server.name()), Some(first));
        path(&[&second_dir, &first_dir]);
        assert_eq!(server_pm(server.name()), Some(second.clone()));
        path(&[]);
        assert_eq!(server_pm(server.name()), None);
        options::run(
            server.name(),
            &[set(Scope::Global, tmux_init::BIN, second.to_str())],
        )
        .unwrap();
        assert_eq!(server_pm(server.name()), Some(second));
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

        let socket = tmux::socket_path(server.name()).unwrap().unwrap();
        let watcher = spawn_watch(&projects_dir, &server);
        let start = std::time::Instant::now();
        while tmux_lock::try_lock(&socket, "watch").unwrap().is_some() {
            assert!(start.elapsed() < Duration::from_secs(10), "watching");
            std::thread::sleep(Duration::from_millis(20));
        }
        tmux::kill_server(server.name()).unwrap();

        watcher
            .recv_timeout(Duration::from_secs(10))
            .expect("server gone")
            .unwrap();
        let files: Vec<_> = std::fs::read_dir(paths::global_runtime_dir().unwrap().join("tmux"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(server.name().unwrap()))
            .collect();
        assert!(files.is_empty(), "{files:?}");
    }
}
