//! The tmux side of a command: its server, its agent's window, and the
//! background refresh of pm's tmux options.

use std::os::unix::process::CommandExt;
use std::process::Stdio;

use pm::commands::attention::AgentState;
use pm::commands::tmux_push::AgentWindow;
use pm::tmux;

/// The agent this command runs in, if any.
pub(super) fn running_agent() -> Option<String> {
    std::env::var("PM_AGENT_NAME")
        .ok()
        .filter(|a| !a.is_empty())
}

/// The tmux server every command in this process targets: `PM_TMUX_SERVER`
/// as a `-L` socket name, or the default server when unset or empty.
pub(super) fn tmux_server_from_env() -> Option<String> {
    std::env::var("PM_TMUX_SERVER")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Leave the user in `session` rather than detached.
pub(super) fn connect(server: Option<&str>, session: &str) {
    let tmux_env = std::env::var("TMUX").ok();
    if let Err(e) = tmux::connect_session(server, session, tmux_env.as_deref()) {
        eprintln!("warning: could not connect to {session}: {e}");
    }
}

/// Push the tmux refresh, killing first the session this process runs in
/// when the command left it to be killed last (`own`).
pub(super) fn finish_in_own_session(
    server: Option<&str>,
    own: Option<tmux::OwnSession>,
) -> pm::error::Result<()> {
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let killed = own.map_or(Ok(()), |own| own.kill(server));
    push();
    killed
}

/// Bring pm's tmux options up to date with a change this command made, in
/// a background `pm tmux push` the command neither waits for nor fails on.
pub(super) fn push() {
    let Ok(pm) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(pm)
        .args(["tmux", "push"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

/// Start `pm harness hooks restart-at-idle` for `agent` of `scope`, in a
/// session of its own: the restart kills the agent's pane, and with it
/// every process of the pane's session. It runs from the project root, and
/// without `PM_AGENT_NAME`, so the restart does not take it for the agent
/// restarting itself.
pub(super) fn restart_at_idle(project_root: &std::path::Path, scope: &str, agent: &str) {
    let Ok(pm) = std::env::current_exe() else {
        return;
    };
    let mut command = std::process::Command::new(pm);
    command
        .args([
            "harness",
            "hooks",
            "restart-at-idle",
            agent,
            "--scope",
            scope,
        ])
        .current_dir(project_root)
        .env_remove("PM_AGENT_NAME")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe, and nothing else runs between
    // the fork and the exec.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = command.spawn();
}

/// The window of `agent`, run in the pane this command runs in.
pub(super) fn agent_window(server: Option<&str>, agent: &str) -> Option<AgentWindow> {
    let pane = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())?;
    Some(AgentWindow::new(server, &pane, agent))
}

/// Write this agent's own window options now, as a hook changing its state
/// does, ahead of the push that refreshes the rest.
pub(super) fn publish(state: AgentState, unread: u32) {
    if let Some(mut window) =
        running_agent().and_then(|agent| agent_window(tmux_server_from_env().as_deref(), &agent))
    {
        window.publish(state, unread);
    }
}
