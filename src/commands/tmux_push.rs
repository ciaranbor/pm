//! `pm tmux push`, and the agent-window write: how pm brings its tmux
//! options up to date when it changes state itself, rather than waiting for
//! the watcher's next poll ([`tmux_watch`](super::tmux_watch)).
//!
//! Commands that change what the snapshot shows start `pm tmux push` in the
//! background as they finish, so a push never delays them. It is a full
//! [`refresh`] under the server's refresh lock, so it alerts no more than
//! the watcher would, and it runs only while a watcher holds the server: a
//! server without the plugin, or with `@pm-auto-refresh off`, is left alone.
//! A command that only spawns agents doesn't push: until the harness
//! starts, its window runs only a shell and reads as dead. The harness's
//! own hooks push instead — SessionStart, where it has, and the Stop hook
//! as the first turn ends.
//!
//! The Stop hook writes its own window's options ([`AgentWindow`]), found
//! by the pane it runs in, in one `tmux` call it does not wait for, as the
//! agent goes idle and as it resumes. That write is ungated and never
//! alerts; the next refresh corrects anything it gets wrong. Going idle
//! also pushes, since an idle team is what makes a feature stalled.

use std::path::Path;
use std::process::Child;

use crate::error::Result;
use crate::tmux::{self, options};

use super::attention::{AgentSnapshot, AgentState};
use super::tmux_refresh::{refresh, window_values};
use super::tmux_watch::try_lock;

/// [`refresh`], while a watcher keeps the server's options current.
pub fn push(projects_dir: &Path, tmux_server: Option<&str>) -> Result<()> {
    let Some(socket) = tmux::socket_path(tmux_server)? else {
        return Ok(());
    };
    if try_lock(&socket, "watch")?.is_some() {
        return Ok(());
    }
    refresh(projects_dir, tmux_server)
}

/// An agent's own window, by the pane it runs in (`$TMUX_PANE`).
pub struct AgentWindow {
    server: Option<String>,
    pane: String,
    agent: String,
    /// The last write, which the next one waits for so they land in order.
    pending: Option<Child>,
}

impl AgentWindow {
    pub fn new(server: Option<&str>, pane: &str, agent: &str) -> Self {
        Self {
            server: server.map(str::to_string),
            pane: pane.to_string(),
            agent: agent.to_string(),
            pending: None,
        }
    }

    /// Publish the agent as `state` with `unread` messages, as a refresh
    /// would, without waiting for the write. Best-effort.
    pub fn publish(&mut self, state: AgentState, unread: u32) {
        if let Some(mut last) = self.pending.take() {
            let _ = last.wait();
        }
        let agent = AgentSnapshot {
            name: self.agent.clone(),
            state,
            unread,
            window: None,
            pane: None,
            waiting: None,
        };
        let scope = options::Scope::PaneWindow(&self.pane);
        let commands: Vec<options::Command> = window_values(&agent)
            .iter()
            .map(|(name, value)| options::set(scope, name, value.as_deref()))
            .collect();
        self.pending = options::spawn(self.server.as_deref(), &commands).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tmux_refresh::WINDOW_OPTIONS;
    use crate::commands::{feat_new, feat_status::feat_status, init, tmux_refresh::lock_file};
    use crate::state::feature::Progress;
    use crate::testing::{OwnServer, TestServer};
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    fn tmux_out(server: Option<&str>, args: &[&str]) -> String {
        let out = std::process::Command::new("tmux")
            .args(["-L", server.unwrap()])
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn window_values(server: &TestServer, window: &str) -> Vec<String> {
        let options = options::read(server.name(), &[], WINDOW_OPTIONS, &[])
            .unwrap()
            .unwrap();
        let held = options.windows.iter().find(|w| w.target == window).unwrap();
        WINDOW_OPTIONS
            .iter()
            .map(|n| held.get(n).to_string())
            .collect()
    }

    #[test]
    fn an_agent_window_is_written_through_its_pane() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let session = server.scope("push/window");
        tmux::create_session(server.name(), &session, dir.path()).unwrap();
        let window =
            tmux::new_window(server.name(), &session, dir.path(), Some("x"), true).unwrap();
        let pane = tmux_out(
            server.name(),
            &["display", "-p", "-t", &window, "#{pane_id}"],
        );

        let mut agent = AgentWindow::new(server.name(), &pane, "reviewer");
        agent.publish(AgentState::Idle, 0);
        agent.publish(AgentState::Busy, 2);

        let want = [
            "reviewer",
            "busy",
            "2",
            "#[fg=green]\u{f013} #[fg=yellow]\u{f0e0}#[default]",
        ];
        let start = Instant::now();
        while window_values(&server, &window) != want {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "{:?}",
                window_values(&server, &window)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let first = format!("{session}:0");
        assert_eq!(
            window_values(&server, &first)[0],
            "",
            "only the pane's own window"
        );
    }

    #[test]
    fn a_push_refreshes_only_a_watched_server() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("push");
        let project = dir.path().join("app");
        let projects_dir = dir.path().join("registry");
        init::init(&project, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();
        feat_status(&project, "login", Progress::Blocked, Some("why?"), None).unwrap();
        let count = || tmux_out(server.name(), &["show", "-gqv", "@pm_count"]);

        push(&projects_dir, server.name()).unwrap();
        assert_eq!(count(), "", "no watcher, no push");

        let socket = tmux::socket_path(server.name()).unwrap().unwrap();
        let watcher = lock_file(&socket, "watch").unwrap();
        watcher.lock().unwrap();
        push(&projects_dir, server.name()).unwrap();
        assert_eq!(count(), "1");
    }
}
