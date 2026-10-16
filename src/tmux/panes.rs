//! An agent's pane, as distinct from its window. A user may split an
//! agent's window and run anything beside it, so whatever replaces or ends
//! the agent acts on its pane ([`mark_agent_pane`]) and leaves the
//! window and the user's panes in place, unless the agent's is the only one.

use std::path::Path;

use super::process::{Process, ProcessTable};
use super::{exact, no_server, run_tmux, run_tmux_untrimmed};
use crate::error::{PmError, Result};

/// The pane option that marks the pane pm started an agent's harness in.
/// tmux renumbers panes as they are split, so neither the active pane nor
/// the first is necessarily the agent's.
const AGENT_PANE: &str = "@pm_agent_pane";

/// Mark the active pane of `window`, a window pm just made for an agent,
/// as the agent's pane.
pub fn mark_agent_pane(server: Option<&str>, window: &str) -> Result<()> {
    run_tmux(
        server,
        &["set-option", "-p", "-t", &exact(window), AGENT_PANE, "1"],
    )?;
    Ok(())
}

/// The processes running in a window's agent pane: the pane's own and its
/// descendants, which is where a harness started from the pane's shell is.
pub fn pane_processes(server: Option<&str>, target: &str) -> Result<Vec<Process>> {
    let Some(pane) = read_agent_pane(server, target)? else {
        return Ok(Vec::new());
    };
    Ok(ProcessTable::read()?.tree(pane.pid))
}

/// `window`'s agent pane, as [`agent_panes`] reads it.
fn read_agent_pane(server: Option<&str>, window: &str) -> Result<Option<Pane>> {
    let output = run_tmux_untrimmed(
        server,
        &["list-panes", "-t", &exact(window), "-F", &pane_format()],
    )?;
    Ok(agent_panes_in(&output).into_iter().next())
}

/// The id of `window`'s agent pane, for commands that would otherwise act
/// on its active pane; the window itself when it has none.
fn agent_pane_target(server: Option<&str>, window: &str) -> Result<String> {
    Ok(read_agent_pane(server, window)?.map_or_else(|| window.to_string(), |p| p.id))
}

/// A window's agent pane, as [`agent_panes`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub session: String,
    pub window_name: String,
    /// The window's target, `session:index`.
    pub window: String,
    /// The pane's id, `%N`.
    pub id: String,
    pub pid: u32,
}

/// What [`agent_panes_in`] reads of each pane. Its last field is empty for
/// an unmarked pane, so its output must not be trimmed.
fn pane_format() -> String {
    format!(
        "#{{session_name}}\t#{{window_name}}\t#{{session_name}}:#{{window_index}}\t#{{pane_id}}\t#{{pane_pid}}\t#{{{AGENT_PANE}}}"
    )
}

/// The agent pane of every window on the server, from one `list-panes -a`.
/// No server running lists none.
pub fn agent_panes(server: Option<&str>) -> Result<Vec<Pane>> {
    match run_tmux_untrimmed(server, &["list-panes", "-a", "-F", &pane_format()]) {
        Ok(output) => Ok(agent_panes_in(&output)),
        Err(PmError::Tmux(msg)) if no_server(&msg) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Each window's marked pane in `output`, else its first, in window order.
fn agent_panes_in(output: &str) -> Vec<Pane> {
    let mut panes: Vec<Pane> = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [session, window_name, window, id, pid, marked] = fields[..] else {
            continue;
        };
        let Ok(pid) = pid.parse() else { continue };
        let pane = Pane {
            session: session.to_string(),
            window_name: window_name.to_string(),
            window: window.to_string(),
            id: id.to_string(),
            pid,
        };
        match panes.iter().position(|p| p.window == window) {
            None => panes.push(pane),
            Some(at) if !marked.is_empty() => panes[at] = pane,
            Some(_) => {}
        }
    }
    panes
}

/// The process name (e.g. "claude", "zsh") in the foreground of a
/// window's agent pane.
pub fn pane_command(server: Option<&str>, window: &str) -> Result<String> {
    let pane = agent_pane_target(server, window)?;
    run_tmux(
        server,
        &[
            "display-message",
            "-p",
            "-t",
            &exact(&pane),
            "#{pane_current_command}",
        ],
    )
}

/// Full scrollback of a window's agent pane as plain text, wrapped lines
/// joined.
pub fn capture_pane(server: Option<&str>, window: &str) -> Result<String> {
    let pane = agent_pane_target(server, window)?;
    run_tmux(
        server,
        &["capture-pane", "-p", "-J", "-S", "-", "-t", &exact(&pane)],
    )
}

/// What `pane` shows now, as plain text, row for row: a wrapped line stays
/// on the rows it was drawn on, so no row is wider than the pane.
pub fn capture_visible(server: Option<&str>, pane: &str) -> Result<String> {
    run_tmux_untrimmed(server, &["capture-pane", "-p", "-t", &exact(pane)])
}

/// The id (`%N`) of `window`'s agent pane: its marked pane, else its first.
pub fn agent_pane(server: Option<&str>, window: &str) -> Result<Option<String>> {
    Ok(read_agent_pane(server, window)?.map(|p| p.id))
}

/// Whether `window` has panes other than its agent's.
pub fn is_split(server: Option<&str>, window: &str) -> Result<bool> {
    let output = run_tmux(
        server,
        &["list-panes", "-t", &exact(window), "-F", "#{pane_id}"],
    )?;
    Ok(output.lines().count() > 1)
}

/// Kill whatever runs in `pane` and start its shell again, in `dir`.
pub fn respawn(server: Option<&str>, pane: &str, dir: &Path) -> Result<()> {
    let dir = dir.to_string_lossy();
    run_tmux(
        server,
        &["respawn-pane", "-k", "-t", &exact(pane), "-c", &dir],
    )?;
    Ok(())
}

/// Split a new pane, running a shell in `dir`, off `pane`, and make it the
/// active one. Returns its id. Once `pane` is killed the new one takes back
/// its whole area.
pub fn split(server: Option<&str>, pane: &str, dir: &Path) -> Result<String> {
    let dir = dir.to_string_lossy();
    run_tmux(
        server,
        &[
            "split-window",
            "-t",
            &exact(pane),
            "-c",
            &dir,
            "-P",
            "-F",
            "#{pane_id}",
        ],
    )
}

pub fn kill(server: Option<&str>, pane: &str) -> Result<()> {
    run_tmux(server, &["kill-pane", "-t", &exact(pane)])?;
    Ok(())
}

/// Whether someone may be using `pane`: it is in a mode (copy mode, or a
/// chooser such as tree mode), where keys drive the mode rather than reach
/// the program, or it is the pane an attached client is looking at, where
/// the user may start typing at any moment. `capture-pane` shows neither: it
/// reads the screen under a mode.
pub fn in_use(server: Option<&str>, pane: &str) -> Result<bool> {
    let out = run_tmux(
        server,
        &[
            "display-message",
            "-p",
            "-t",
            &exact(pane),
            "#{||:#{pane_in_mode},#{&&:#{pane_active},#{&&:#{window_active},#{session_attached}}}}",
        ],
    )?;
    Ok(out == "1")
}

/// End the agent in `window`: kill the window, or, when the user has split
/// it, only the agent's pane. A window left behind is named after what its
/// panes run, no longer after the agent.
pub fn end_agent(server: Option<&str>, window: &str) -> Result<()> {
    match agent_pane(server, window)? {
        Some(pane) if is_split(server, window)? => {
            run_tmux(
                server,
                &[
                    "set-option",
                    "-w",
                    "-t",
                    &exact(window),
                    "automatic-rename",
                    "on",
                ],
            )?;
            kill(server, &pane)
        }
        _ => super::kill_window(server, window),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use crate::tmux::{self, create_session, send_line, send_text, tmux_command};
    use tempfile::tempdir;

    #[test]
    fn a_window_is_read_by_its_marked_pane_else_its_first() {
        let output = "s\tw\ts:1\t%1\t11\t\n\
                      s\tw\ts:1\t%2\t12\t1\n\
                      s\tw\ts:1\t%3\t13\t\n\
                      s\tx\ts:2\t%4\t14\t\n\
                      s\tx\ts:2\t%5\t15\t\n";
        let panes = agent_panes_in(output);
        let read: Vec<(&str, &str)> = panes
            .iter()
            .map(|p| (p.window.as_str(), p.id.as_str()))
            .collect();
        assert_eq!(read, [("s:1", "%2"), ("s:2", "%4")]);
    }

    #[test]
    fn panes_are_listed_when_the_caller_has_no_utf8_locale() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let session = server.scope("no-locale");
        create_session(server.name(), &session, dir.path()).unwrap();

        let output = tmux_command(server.name())
            .env_remove("LANG")
            .env_remove("LC_ALL")
            .env_remove("LC_CTYPE")
            .env_remove("TMUX")
            .args(["list-panes", "-a", "-F", &pane_format()])
            .output()
            .unwrap();
        assert!(output.status.success());
        let panes = agent_panes_in(&String::from_utf8_lossy(&output.stdout));
        assert!(panes.iter().any(|p| p.session == session), "{panes:?}");
    }

    #[test]
    fn pane_helpers_read_the_agent_pane_whichever_is_active() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let session = server.scope("agent-pane");
        create_session(server.name(), &session, dir.path()).unwrap();
        let window = format!("{session}:0");
        mark_agent_pane(server.name(), &window).unwrap();
        send_line(server.name(), &window, "exec sleep 999").unwrap();
        server.split_before(&window);
        send_text(server.name(), &window, "echo in-the-users-pane").unwrap();

        server.wait_for_pane_text(&window, "exec sleep 999");
        let start = std::time::Instant::now();
        while pane_command(server.name(), &window).unwrap() != "sleep" {
            assert!(start.elapsed() < std::time::Duration::from_secs(10));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            !capture_pane(server.name(), &window)
                .unwrap()
                .contains("in-the-users-pane")
        );
    }

    #[test]
    fn the_visible_screen_keeps_a_wrapped_line_on_the_rows_it_was_drawn_on() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let session = server.scope("wrapped");
        create_session(server.name(), &session, dir.path()).unwrap();
        let window = format!("{session}:0");
        let width: usize = server
            .tmux_stdout(&["display-message", "-p", "-t", &window, "#{pane_width}"])
            .parse()
            .unwrap();
        send_line(
            server.name(),
            &window,
            &format!("printf '%0{}d\\n' 0", width + 10),
        )
        .unwrap();
        server.wait_for_pane_text(&window, "0000000000");

        let screen = capture_visible(server.name(), &window).unwrap();
        assert!(
            screen.lines().all(|l| l.chars().count() <= width),
            "{screen}"
        );
        assert!(screen.lines().any(|l| l == "0".repeat(width)), "{screen}");
    }

    #[test]
    fn pane_command_returns_shell() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("pane-cmd");

        create_session(server.name(), &name, dir.path()).unwrap();
        let target = format!("{name}:0");

        let cmd = pane_command(server.name(), &target).unwrap();
        // Default pane runs a shell (bash, zsh, etc.)
        assert!(!cmd.is_empty());
        assert_ne!(cmd, "claude");
    }

    #[test]
    fn ending_a_split_agent_window_keeps_the_users_pane_and_drops_the_agents_name() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let session = server.scope("panes");
        tmux::create_session(server.name(), &session, dir.path()).unwrap();
        let window =
            tmux::new_window(server.name(), &session, dir.path(), Some("agent"), true).unwrap();
        tmux::mark_agent_pane(server.name(), &window).unwrap();
        let agent = server.pane_id(&window);
        let user = server.split_before(&window);

        end_agent(server.name(), &window).unwrap();

        let left = server.tmux_stdout(&["list-panes", "-t", &window, "-F", "#{pane_id}"]);
        assert_eq!(left, user);
        assert_ne!(left, agent);
        assert_eq!(
            tmux::find_window(server.name(), &session, "agent").unwrap(),
            None
        );

        let lone =
            tmux::new_window(server.name(), &session, dir.path(), Some("solo"), true).unwrap();
        end_agent(server.name(), &lone).unwrap();
        assert_eq!(
            tmux::find_window(server.name(), &session, "solo").unwrap(),
            None
        );
    }
}
