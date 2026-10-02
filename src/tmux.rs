use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};

pub mod keys;
pub mod options;
pub mod panes;

/// Single source of truth for the tmux session naming convention.
/// Returns `"{project_name}/{scope}"`.
pub fn session_name(project_name: &str, scope: &str) -> String {
    format!("{project_name}/{scope}")
}

fn run_tmux(server: Option<&str>, args: &[&str]) -> Result<String> {
    run_tmux_untrimmed(server, args).map(|out| out.trim().to_string())
}

/// `tmux`, aimed at `server`.
fn tmux_command(server: Option<&str>) -> Command {
    let mut cmd = Command::new("tmux");
    if let Some(s) = server {
        cmd.args(["-L", s]);
    }
    cmd
}

/// [`run_tmux`]'s output as tmux printed it.
fn run_tmux_untrimmed(server: Option<&str>, args: &[&str]) -> Result<String> {
    let mut cmd = tmux_command(server);
    cmd.args(args);

    let output = cmd.output()?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        // "no server running" or "session not found" are not hard errors for has_session
        Err(PmError::Tmux(stderr))
    }
}

/// Create a new detached tmux session with the given name and start directory.
pub fn create_session(server: Option<&str>, name: &str, start_dir: &Path) -> Result<()> {
    run_tmux(
        server,
        &[
            "new-session",
            "-d",
            "-s",
            name,
            "-c",
            &start_dir.to_string_lossy(),
        ],
    )?;
    Ok(())
}

/// Check if a tmux session exists.
pub fn has_session(server: Option<&str>, name: &str) -> Result<bool> {
    let result = run_tmux(server, &["has-session", "-t", name]);
    match result {
        Ok(_) => Ok(true),
        Err(PmError::Tmux(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Kill a tmux session.
pub fn kill_session(server: Option<&str>, name: &str) -> Result<()> {
    run_tmux(server, &["kill-session", "-t", name])?;
    Ok(())
}

/// List all tmux session names.
pub fn list_sessions(server: Option<&str>) -> Result<Vec<String>> {
    let result = run_tmux(server, &["list-sessions", "-F", "#{session_name}"]);
    match result {
        Ok(output) => {
            if output.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(output.lines().map(|s| s.to_string()).collect())
            }
        }
        Err(PmError::Tmux(msg)) if no_server(&msg) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Switch the current tmux client to a session.
pub fn switch_client(server: Option<&str>, name: &str) -> Result<()> {
    run_tmux(server, &["switch-client", "-t", name])?;
    Ok(())
}

/// Switch `client` to `target`, keeping its window zoom.
pub fn switch_client_of(server: Option<&str>, client: &str, target: &str) -> Result<()> {
    run_tmux(server, &["switch-client", "-c", client, "-Z", "-t", target])?;
    Ok(())
}

/// The server's socket, which names it whatever way it was reached; `None`
/// when no server is running, or one exiting answered nothing.
pub fn socket_path(server: Option<&str>) -> Result<Option<String>> {
    match run_tmux(server, &["display-message", "-p", "#{socket_path}"]) {
        Ok(path) if path.is_empty() => Ok(None),
        Ok(path) => Ok(Some(path)),
        Err(PmError::Tmux(msg)) if no_server(&msg) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Attach the current terminal to a tmux session. Inherits stdio so tmux takes
/// over the controlling terminal; returns when the user detaches. From a pane
/// of another server the client nests in that pane.
pub fn attach_session(server: Option<&str>, name: &str) -> Result<()> {
    let mut cmd = Command::new("tmux");
    if let Some(s) = server {
        cmd.args(["-L", s]);
    }
    cmd.args(["attach-session", "-t", name]).env_remove("TMUX");
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(PmError::Tmux(format!("attach-session failed for {name}")))
    }
}

/// Connect the current terminal to `session`. `tmux_env` is the caller's
/// `$TMUX`: a client of `server` is switched to it; anything else, a pane of
/// another server included, attaches a fresh client.
pub fn connect_session(server: Option<&str>, session: &str, tmux_env: Option<&str>) -> Result<()> {
    match tmux_env {
        Some(env) if is_client_of(server, session, env) => switch_client(server, session),
        _ => attach_session(server, session),
    }
}

/// Whether `tmux_env` (a `$TMUX` value: `socket,pid,session`) names the
/// socket of the server holding `session`.
fn is_client_of(server: Option<&str>, session: &str, tmux_env: &str) -> bool {
    let ours = tmux_env.split(',').next().unwrap_or_default();
    let Ok(theirs) = run_tmux(
        server,
        &["display-message", "-p", "-t", session, "#{socket_path}"],
    ) else {
        return false;
    };
    let canonical = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| p.into());
    canonical(ours) == canonical(&theirs)
}

/// Create a new window in an existing tmux session. Returns the new window's target
/// (e.g. "session:1") for use with send_line.
/// When `detached` is true, the new window is created without switching to it.
pub fn new_window(
    server: Option<&str>,
    session: &str,
    start_dir: &Path,
    name: Option<&str>,
    detached: bool,
) -> Result<String> {
    let dir_lossy = start_dir.to_string_lossy();
    let mut args = vec!["new-window"];
    if detached {
        args.push("-d");
    }
    args.extend_from_slice(&[
        "-t",
        session,
        "-P",
        "-F",
        "#{session_name}:#{window_index}",
        "-c",
        &dir_lossy,
    ]);
    if let Some(n) = name {
        args.push("-n");
        args.push(n);
    }
    run_tmux(server, &args)
}

/// Count the number of windows in a tmux session.
pub fn list_windows(server: Option<&str>, session: &str) -> Result<usize> {
    let output = run_tmux(
        server,
        &["list-windows", "-t", session, "-F", "#{window_index}"],
    )?;
    Ok(output.lines().count())
}

/// Type `text` into `target` as literal keys, then press Enter: a command
/// line for a shell.
pub fn send_line(server: Option<&str>, target: &str, text: &str) -> Result<()> {
    run_tmux(server, &["send-keys", "-t", target, "-l", text])?;
    send_key(server, target, "Enter")
}

/// [`send_line`] for a harness's input box. The pause before Enter is what
/// makes it submit: codex takes an Enter arriving right after a burst of
/// keys as part of a paste, a newline.
pub fn send_text(server: Option<&str>, target: &str, text: &str) -> Result<()> {
    run_tmux(server, &["send-keys", "-t", target, "-l", text])?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    send_key(server, target, "Enter")
}

/// Press one key, by tmux's name for it (`Enter`, `C-c`), in `target`.
pub fn send_key(server: Option<&str>, target: &str, key: &str) -> Result<()> {
    run_tmux(server, &["send-keys", "-t", target, key])?;
    Ok(())
}

/// The visible screen of `target`'s pane, with the escape sequences that
/// style it.
pub fn capture_screen(server: Option<&str>, target: &str) -> Result<String> {
    run_tmux(server, &["capture-pane", "-p", "-e", "-t", target])
}

/// Find a window by name in a session. Returns the window target (e.g. "session:1") if found.
pub fn find_window(server: Option<&str>, session: &str, name: &str) -> Result<Option<String>> {
    let output = run_tmux(
        server,
        &[
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_name}\t#{session_name}:#{window_index}",
        ],
    )?;
    for line in output.lines() {
        if let Some((wname, target)) = line.split_once('\t')
            && wname == name
        {
            return Ok(Some(target.to_string()));
        }
    }
    Ok(None)
}

/// Find a named window in a session, or create it if it doesn't exist.
pub fn find_or_create_window(
    server: Option<&str>,
    session: &str,
    name: &str,
    start_dir: &Path,
) -> Result<String> {
    if let Some(target) = find_window(server, session, name)? {
        Ok(target)
    } else {
        new_window(server, session, start_dir, Some(name), true)
    }
}

/// Get the session name that the current tmux client is attached to.
/// Uses `#{client_session}` rather than `#{session_name}` so it returns
/// where the client is *currently viewing*, not the session of the pane
/// running this command.
/// Returns `None` if there is no attached client (e.g. running outside tmux).
pub fn current_session(server: Option<&str>) -> Option<String> {
    run_tmux(server, &["display-message", "-p", "#{client_session}"]).ok()
}

/// Shell-quote a string for safe use in send_line (single-quote wrapping with escaping).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Rename a window in a tmux session.
pub fn rename_window(server: Option<&str>, target: &str, new_name: &str) -> Result<()> {
    run_tmux(server, &["rename-window", "-t", target, new_name])?;
    Ok(())
}

/// Kill a specific tmux window.
pub fn kill_window(server: Option<&str>, target: &str) -> Result<()> {
    run_tmux(server, &["kill-window", "-t", target])?;
    Ok(())
}

/// A process as `ps` saw it. The start time tells a pid's process apart
/// from a later one the pid was reused for.
#[derive(Debug, Clone)]
pub struct Process {
    pub pid: u32,
    started: String,
    /// The command line, its arguments separated by single spaces.
    pub command: String,
    /// Whether it is in its terminal's foreground process group: for a
    /// pane's shell, whether it is at its prompt rather than running a job.
    pub foreground: bool,
}

/// A process can rewrite its own command line, so it is not part of what
/// makes two sightings the same process.
impl PartialEq for Process {
    fn eq(&self, other: &Self) -> bool {
        self.pid == other.pid && self.started == other.started
    }
}

impl Eq for Process {}

#[cfg(test)]
impl Process {
    pub fn new_for_test(pid: u32, command: &str) -> Self {
        Self {
            pid,
            started: String::new(),
            command: command.to_string(),
            foreground: true,
        }
    }
}

/// Every process on the machine, read once so that several panes can be
/// walked against one `ps` run.
pub struct ProcessTable(Vec<(Process, u32)>);

impl ProcessTable {
    pub fn read() -> Result<Self> {
        let table = Command::new("ps")
            .args(["-A", "-o", "pid=,ppid=,pgid=,tpgid=,lstart=,command="])
            .output()?;
        Ok(Self(
            String::from_utf8_lossy(&table.stdout)
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    let pid = fields.next()?.parse().ok()?;
                    let ppid = fields.next()?.parse().ok()?;
                    let pgid: i64 = fields.next()?.parse().ok()?;
                    let tpgid: i64 = fields.next()?.parse().ok()?;
                    // `lstart` is always five fields: `Wed Oct  1 16:47:56 2026`.
                    let started: Vec<&str> = fields.by_ref().take(5).collect();
                    if started.len() < 5 {
                        return None;
                    }
                    let process = Process {
                        pid,
                        started: started.join(" "),
                        command: fields.collect::<Vec<_>>().join(" "),
                        foreground: pgid == tpgid,
                    };
                    Some((process, ppid))
                })
                .collect(),
        ))
    }

    /// The process `root` and its descendants.
    pub fn tree(&self, root: u32) -> Vec<Process> {
        let mut found: Vec<Process> = self
            .0
            .iter()
            .filter(|(process, _)| process.pid == root)
            .map(|(process, _)| process.clone())
            .collect();
        let mut at = 0;
        while at < found.len() {
            let parent = found[at].pid;
            for (process, ppid) in &self.0 {
                if *ppid == parent && !found.contains(process) {
                    found.push(process.clone());
                }
            }
            at += 1;
        }
        found
    }
}

/// The pane option that marks the pane pm started an agent's harness in.
/// A user may split an agent's window and run anything beside it, and tmux
/// renumbers panes as they are split, so neither the active pane nor the
/// first is necessarily the agent's.
const AGENT_PANE: &str = "@pm_agent_pane";

/// Mark the active pane of `window`, a window pm just made for an agent,
/// as the agent's pane.
pub fn mark_agent_pane(server: Option<&str>, window: &str) -> Result<()> {
    run_tmux(server, &["set-option", "-p", "-t", window, AGENT_PANE, "1"])?;
    Ok(())
}

/// The processes running in a window's agent pane: the pane's own and its
/// descendants, which is where a harness started from the pane's shell is.
pub fn pane_processes(server: Option<&str>, target: &str) -> Result<Vec<Process>> {
    let Some(pane) = agent_pane(server, target)? else {
        return Ok(Vec::new());
    };
    Ok(ProcessTable::read()?.tree(pane.pid))
}

/// `window`'s agent pane, as [`agent_panes`] reads it.
fn agent_pane(server: Option<&str>, window: &str) -> Result<Option<Pane>> {
    let output = run_tmux_untrimmed(server, &["list-panes", "-t", window, "-F", &pane_format()])?;
    Ok(agent_panes_in(&output).into_iter().next())
}

/// The id of `window`'s agent pane, for commands that would otherwise act
/// on its active pane; the window itself when it has none.
fn agent_pane_target(server: Option<&str>, window: &str) -> Result<String> {
    Ok(agent_pane(server, window)?.map_or_else(|| window.to_string(), |p| p.id))
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

/// Whether a tmux error says there is no server to talk to: none was
/// running (the message varies by platform), or it exited mid-command.
fn no_server(msg: &str) -> bool {
    [
        "no server running",
        "error connecting",
        "server exited unexpectedly",
        "lost server",
    ]
    .iter()
    .any(|m| msg.contains(m))
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
            &pane,
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
        &["capture-pane", "-p", "-J", "-S", "-", "-t", &pane],
    )
}

/// Select (focus) a specific window in a session.
pub fn select_window(server: Option<&str>, target: &str) -> Result<()> {
    run_tmux(server, &["select-window", "-t", target])?;
    Ok(())
}

/// Return the name of the currently active window in a session, or `None`
/// if the session has no active window / does not exist.
pub fn active_window_name(server: Option<&str>, session: &str) -> Result<Option<String>> {
    let output = run_tmux(
        server,
        &[
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_active}\t#{window_name}",
        ],
    )?;
    for line in output.lines() {
        if let Some((active, name)) = line.split_once('\t')
            && active == "1"
        {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

/// Kill the entire tmux server (used in tests for cleanup).
pub fn kill_server(server: Option<&str>) -> Result<()> {
    let _ = run_tmux(server, &["kill-server"]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
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
    fn create_session_is_visible_in_list() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("test-session");

        create_session(server.name(), &name, dir.path()).unwrap();

        let sessions = list_sessions(server.name()).unwrap();
        assert!(sessions.contains(&name));
    }

    #[test]
    fn has_session_returns_true_for_existing() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("exists-session");

        create_session(server.name(), &name, dir.path()).unwrap();

        assert!(has_session(server.name(), &name).unwrap());
    }

    #[test]
    fn has_session_returns_false_for_nonexistent() {
        let server = TestServer::new();

        assert!(!has_session(server.name(), &server.scope("no-such-session")).unwrap());
    }

    #[test]
    fn kill_session_removes_session() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("kill-me");

        create_session(server.name(), &name, dir.path()).unwrap();
        assert!(has_session(server.name(), &name).unwrap());

        kill_session(server.name(), &name).unwrap();
        assert!(!has_session(server.name(), &name).unwrap());
    }

    #[test]
    fn list_sessions_returns_all_sessions() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let a = server.scope("list-a");
        let b = server.scope("list-b");

        create_session(server.name(), &a, dir.path()).unwrap();
        create_session(server.name(), &b, dir.path()).unwrap();

        let sessions = list_sessions(server.name()).unwrap();
        assert!(sessions.contains(&a));
        assert!(sessions.contains(&b));
    }

    #[test]
    fn list_sessions_returns_empty_when_no_server() {
        // Use a PID-unique server name (not the shared test server) to exercise
        // the "no server running" code path.
        let name = format!("pm-test-{}-never", std::process::id());
        let sessions = list_sessions(Some(&name)).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn new_window_creates_second_window() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("win-test");

        create_session(server.name(), &name, dir.path()).unwrap();
        let target = new_window(server.name(), &name, dir.path(), None, false).unwrap();

        // Should return a target like "<name>:1"
        assert!(target.starts_with(&format!("{name}:")));

        // Session should still exist and now have 2 windows
        assert!(has_session(server.name(), &name).unwrap());
        let output = run_tmux(
            server.name(),
            &["list-windows", "-t", &name, "-F", "#{window_index}"],
        )
        .unwrap();
        assert_eq!(output.lines().count(), 2);
    }

    #[test]
    fn new_window_nonexistent_session_fails() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();

        let result = new_window(
            server.name(),
            &server.scope("no-such"),
            dir.path(),
            None,
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn list_windows_counts_windows() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("count-test");

        create_session(server.name(), &name, dir.path()).unwrap();
        assert_eq!(list_windows(server.name(), &name).unwrap(), 1);

        new_window(server.name(), &name, dir.path(), None, false).unwrap();
        assert_eq!(list_windows(server.name(), &name).unwrap(), 2);
    }

    #[test]
    fn list_windows_nonexistent_session_fails() {
        let server = TestServer::new();

        let result = list_windows(server.name(), &server.scope("no-such"));
        assert!(result.is_err());
    }

    #[test]
    fn switch_client_without_attached_client_returns_tmux_error() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("target");

        create_session(server.name(), &name, dir.path()).unwrap();

        let result = switch_client(server.name(), &name);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::Tmux(_)));
    }

    #[test]
    fn a_client_is_of_the_server_whose_socket_its_tmux_env_names() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("client-of");
        create_session(server.name(), &name, dir.path()).unwrap();
        let socket = run_tmux(
            server.name(),
            &["display-message", "-p", "-t", &name, "#{socket_path}"],
        )
        .unwrap();

        assert!(is_client_of(server.name(), &name, &format!("{socket},1,0")));
        let other = std::path::Path::new(&socket).with_file_name("pm-elsewhere");
        assert!(!is_client_of(
            server.name(),
            &name,
            &format!("{},1,0", other.display())
        ));
    }

    #[test]
    fn a_line_that_names_a_key_is_typed_as_text() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("literal");
        create_session(server.name(), &name, dir.path()).unwrap();

        send_line(server.name(), &name, "C-c").unwrap();
        send_line(server.name(), &name, "echo typed-after").unwrap();

        server.wait_for_pane_text(&name, "typed-after\n");
        let text = capture_pane(server.name(), &name).unwrap();
        assert!(text.lines().any(|l| l.ends_with("C-c")), "{text}");
    }

    #[test]
    fn send_line_to_nonexistent_session_fails() {
        let server = TestServer::new();

        let result = send_line(server.name(), &server.scope("nonexistent"), "echo hello");
        assert!(result.is_err());
    }

    #[test]
    fn rename_window_changes_window_name() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("rename-win");

        create_session(server.name(), &name, dir.path()).unwrap();
        // Default window is at :0
        let target = format!("{name}:0");
        rename_window(server.name(), &target, "agent").unwrap();

        // The window should now be findable by the new name
        let found = find_window(server.name(), &name, "agent").unwrap();
        assert!(found.is_some());
    }

    #[test]
    fn kill_window_removes_window() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("kill-win");

        create_session(server.name(), &name, dir.path()).unwrap();
        // Create a second window so killing one doesn't destroy the session
        let target = new_window(server.name(), &name, dir.path(), Some("doomed"), true).unwrap();

        assert_eq!(list_windows(server.name(), &name).unwrap(), 2);
        kill_window(server.name(), &target).unwrap();
        assert_eq!(list_windows(server.name(), &name).unwrap(), 1);
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
    fn shell_quote_wraps_in_single_quotes() {
        assert_eq!(shell_quote("hello"), "'hello'");
    }

    #[test]
    fn shell_quote_handles_spaces() {
        assert_eq!(shell_quote("/path/to/my hook.sh"), "'/path/to/my hook.sh'");
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
