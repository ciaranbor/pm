use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};

fn run_tmux(server: Option<&str>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("tmux");
    if let Some(s) = server {
        cmd.args(["-L", s]);
    }
    cmd.args(args);

    let output = cmd.output()?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

/// Rename a tmux session.
pub fn rename_session(server: Option<&str>, old_name: &str, new_name: &str) -> Result<()> {
    run_tmux(server, &["rename-session", "-t", old_name, new_name])?;
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
        // No server running = no sessions (message varies by platform)
        Err(PmError::Tmux(msg))
            if msg.contains("no server running") || msg.contains("error connecting") =>
        {
            Ok(Vec::new())
        }
        Err(e) => Err(e),
    }
}

/// Switch the current tmux client to a session.
/// Returns the command args for tmux switch-client (for use in display-menu or direct execution).
pub fn switch_client(server: Option<&str>, name: &str) -> Result<()> {
    run_tmux(server, &["switch-client", "-t", name])?;
    Ok(())
}

/// Create a new window in an existing tmux session. Returns the new window's target
/// (e.g. "session:1") for use with send_keys.
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

/// Send keys to a tmux session (for running commands like setup.sh).
pub fn send_keys(server: Option<&str>, target: &str, keys: &str) -> Result<()> {
    run_tmux(server, &["send-keys", "-t", target, keys, "Enter"])?;
    Ok(())
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
/// Returns `None` if there is no attached client (e.g. running outside tmux).
pub fn current_session(server: Option<&str>) -> Option<String> {
    run_tmux(server, &["display-message", "-p", "#{session_name}"]).ok()
}

/// Shell-quote a string for safe use in send_keys (single-quote wrapping with escaping).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Show a tmux display-menu for selecting from a list of items.
/// Each item is a (label, session_name) pair. Selecting an item switches to that session.
pub fn display_menu(server: Option<&str>, title: &str, items: &[(String, String)]) -> Result<()> {
    let args = build_display_menu_args(title, items);
    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    // display-menu fails silently outside tmux — that's acceptable
    let _ = run_tmux(server, &args_refs);
    Ok(())
}

/// Build tmux display-menu arguments for a list of items.
/// Each item gets a shortcut key (1-9, a-z).
fn build_display_menu_args(
    title: &str,
    items: &[(String, String)], // (label, session_name)
) -> Vec<String> {
    let mut args = vec![
        "display-menu".to_string(),
        "-T".to_string(),
        title.to_string(),
    ];

    let shortcuts: Vec<char> = "123456789abcdefghijklmnopqrstuvwxyz".chars().collect();

    for (i, (label, session_name)) in items.iter().enumerate() {
        let key = shortcuts.get(i).map(|c| c.to_string()).unwrap_or_default();
        args.push(label.clone());
        args.push(key);
        args.push(format!("switch-client -t '{session_name}'"));
    }

    args
}

/// Rename a window in a tmux session.
pub fn rename_window(server: Option<&str>, target: &str, new_name: &str) -> Result<()> {
    run_tmux(server, &["rename-window", "-t", target, new_name])?;
    Ok(())
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
    fn build_display_menu_args_creates_correct_structure() {
        let items = vec![
            ("login".to_string(), "myapp/login".to_string()),
            ("api".to_string(), "myapp/api".to_string()),
        ];

        let args = build_display_menu_args("Features", &items);

        assert_eq!(args[0], "display-menu");
        assert_eq!(args[1], "-T");
        assert_eq!(args[2], "Features");
        assert_eq!(args[3], "login");
        assert_eq!(args[4], "1");
        assert_eq!(args[5], "switch-client -t 'myapp/login'");
        assert_eq!(args[6], "api");
        assert_eq!(args[7], "2");
        assert_eq!(args[8], "switch-client -t 'myapp/api'");
    }

    #[test]
    fn build_display_menu_args_empty_list() {
        let items: Vec<(String, String)> = vec![];
        let args = build_display_menu_args("Empty", &items);

        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "display-menu");
        assert_eq!(args[2], "Empty");
    }

    #[test]
    fn build_display_menu_args_many_items_uses_empty_shortcut_past_limit() {
        let items: Vec<(String, String)> = (0..40)
            .map(|i| (format!("item-{i}"), format!("session-{i}")))
            .collect();

        let args = build_display_menu_args("Big", &items);

        assert_eq!(args[4], "1");
        // Item at index 35 (0-indexed), past the 34 shortcuts available
        let shortcut_pos = 3 + 35 * 3 + 1;
        assert_eq!(args[shortcut_pos], "");
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
    fn send_keys_to_existing_session_succeeds() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let name = server.scope("keys-test");

        create_session(server.name(), &name, dir.path()).unwrap();

        let result = send_keys(server.name(), &name, "echo hello");
        assert!(result.is_ok());
    }

    #[test]
    fn send_keys_to_nonexistent_session_fails() {
        let server = TestServer::new();

        let result = send_keys(server.name(), &server.scope("nonexistent"), "echo hello");
        assert!(result.is_err());
    }

    #[test]
    fn display_menu_returns_ok_even_outside_tmux() {
        let server = TestServer::new();
        let name = server.scope("myapp");
        let items = vec![("login".to_string(), format!("{name}/login"))];

        // display_menu swallows the tmux error (no client attached)
        let result = display_menu(server.name(), "Test", &items);
        assert!(result.is_ok());
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
