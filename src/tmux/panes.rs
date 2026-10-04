//! An agent's pane, as distinct from its window. A user may split an
//! agent's window and run anything beside it, so whatever replaces or ends
//! the agent acts on its pane ([`super::mark_agent_pane`]) and leaves the
//! window and the user's panes in place, unless the agent's is the only one.

use std::path::Path;

use super::{agent_panes_in, exact, pane_format, run_tmux, run_tmux_untrimmed};
use crate::error::Result;

/// The id (`%N`) of `window`'s agent pane: its marked pane, else its first.
pub fn agent_pane(server: Option<&str>, window: &str) -> Result<Option<String>> {
    let output = run_tmux_untrimmed(
        server,
        &["list-panes", "-t", &exact(window), "-F", &pane_format()],
    )?;
    Ok(agent_panes_in(&output).into_iter().next().map(|p| p.id))
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
    use crate::tmux;
    use tempfile::tempdir;

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
