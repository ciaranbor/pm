//! User options (`@name`) read and written in bulk: [`read`] takes every
//! session's and window's values, the global ones and the attached clients
//! in one `tmux` call, and [`run`] sends any number of commands in one.
//!
//! Values go through tmux formats, so they are text, not markup, only once
//! [`format_text`] has escaped them: a `#` in a value would otherwise be read
//! as a style, or a shell command, wherever the value is displayed.

use std::collections::BTreeMap;

use super::{no_server, run_tmux};
use crate::error::{PmError, Result};

/// One tmux command and its arguments.
pub type Command = Vec<String>;

/// A session's or window's values of the requested options; an unset option
/// reads as empty.
#[derive(Debug, Default)]
pub struct Holder {
    /// The session's name, or the window's target, `session:index`.
    pub target: String,
    /// The window's session; the session itself for a session.
    pub session: String,
    values: BTreeMap<String, String>,
}

impl Holder {
    pub fn get(&self, name: &str) -> &str {
        self.values.get(name).map_or("", String::as_str)
    }
}

#[derive(Debug, Default)]
pub struct Options {
    /// Attached clients, by name.
    pub clients: Vec<String>,
    pub sessions: Vec<Holder>,
    pub windows: Vec<Holder>,
    pub global: Holder,
}

/// Every attached client, and the values of `session_names` on every
/// session, `window_names` on every window and `global_names` globally.
/// `None` when no server is running. tmux resolves a name a window doesn't
/// set to its session's value and one a session doesn't set to the global
/// value, so each list must hold names the wider scopes don't set.
pub fn read(
    server: Option<&str>,
    session_names: &[&str],
    window_names: &[&str],
    global_names: &[&str],
) -> Result<Option<Options>> {
    let fields =
        |names: &[&str]| -> String { names.iter().map(|n| format!("\t#{{{n}}}")).collect() };
    let sessions = format!(
        "S\t#{{session_name}}\t#{{session_name}}{}",
        fields(session_names)
    );
    let windows = format!(
        "W\t#{{session_name}}:#{{window_index}}\t#{{session_name}}{}",
        fields(window_names)
    );
    let global = format!("G\t\t{}", fields(global_names));
    let output = match run_tmux(
        server,
        &[
            "list-clients",
            "-F",
            "C\t#{client_name}",
            ";",
            "list-sessions",
            "-F",
            &sessions,
            ";",
            "list-windows",
            "-a",
            "-F",
            &windows,
            ";",
            "display-message",
            "-p",
            &global,
        ],
    ) {
        Ok(output) => output,
        Err(PmError::Tmux(msg)) if no_server(&msg) => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut options = Options::default();
    for line in output.lines() {
        let mut fields = line.split('\t');
        let kind = fields.next();
        if kind == Some("C") {
            options.clients.extend(fields.next().map(str::to_string));
            continue;
        }
        let names = match kind {
            Some("W") => window_names,
            Some("G") => global_names,
            _ => session_names,
        };
        let mut holder = Holder {
            target: fields.next().unwrap_or_default().to_string(),
            session: fields.next().unwrap_or_default().to_string(),
            values: BTreeMap::new(),
        };
        for (name, value) in names.iter().zip(fields) {
            holder.values.insert(name.to_string(), value.to_string());
        }
        match kind {
            Some("S") => options.sessions.push(holder),
            Some("W") => options.windows.push(holder),
            Some("G") => options.global = holder,
            _ => {}
        }
    }
    Ok(Some(options))
}

/// Where an option is set.
#[derive(Debug, Clone, Copy)]
pub enum Scope<'a> {
    Global,
    /// A session, by name.
    Session(&'a str),
    /// A window, by target (`session:index`).
    Window(&'a str),
}

/// Set `name` in `scope`, or unset it when `value` is `None`. A target that
/// has gone is skipped rather than failing the commands after it.
pub fn set(scope: Scope, name: &str, value: Option<&str>) -> Command {
    let mut command = vec!["set-option".to_string(), "-q".to_string()];
    match scope {
        Scope::Global => command.push("-g".into()),
        // `=` takes the name exactly rather than as a prefix; the trailing
        // `:` makes the target a session rather than a window or pane.
        Scope::Session(session) => command.extend(["-t".into(), format!("={session}:")]),
        Scope::Window(window) => command.extend(["-w".into(), "-t".into(), format!("={window}")]),
    }
    match value {
        Some(value) => command.extend([name.to_string(), value.to_string()]),
        None => command.extend(["-u".to_string(), name.to_string()]),
    }
    command
}

/// Show `text` on `client`'s status line.
pub fn display(client: &str, text: &str) -> Command {
    vec![
        "display-message".into(),
        "-c".into(),
        client.into(),
        format_text(text),
    ]
}

/// Redraw `client`'s status line; a mode open in the client, such as tree
/// mode, rebuilds with it.
pub fn refresh_status(client: &str) -> Command {
    vec![
        "refresh-client".into(),
        "-S".into(),
        "-t".into(),
        client.into(),
    ]
}

/// Run `commands` in one `tmux` invocation, in order. A failing command
/// skips the rest.
pub fn run(server: Option<&str>, commands: &[Command]) -> Result<()> {
    let mut args: Vec<String> = Vec::new();
    for command in commands {
        if !args.is_empty() {
            args.push(";".into());
        }
        args.extend(command.iter().map(|arg| separator_safe(arg)));
    }
    if args.is_empty() {
        return Ok(());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_tmux(server, &args).map(|_| ())
}

/// `arg` as it must be passed for tmux not to read a trailing `;` as the end
/// of the command.
fn separator_safe(arg: &str) -> String {
    match arg.strip_suffix(';') {
        Some(head) => format!("{head}\\;"),
        None => arg.to_string(),
    }
}

/// `text` as a format shows it literally, on one line: a `#` is doubled so
/// it cannot start a style, a format or a `#(command)`.
pub fn format_text(text: &str) -> String {
    text.replace('#', "##")
        .replace(['\t', '\n', '\r'], " ")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn values_survive_the_round_trip_whatever_they_contain() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let session = server.scope("opts/round-trip");
        let prefixed = format!("{session}-longer");
        super::super::create_session(server.name(), &prefixed, dir.path()).unwrap();
        super::super::create_session(server.name(), &session, dir.path()).unwrap();
        let tricky = ["ends in;", "x\\;", "#[fg=red]styled", "-dash", ";"];

        let commands: Vec<Command> = tricky
            .iter()
            .enumerate()
            .map(|(i, value)| set(Scope::Session(&session), &format!("@pm_t{i}"), Some(value)))
            .chain([set(Scope::Session(&session), "@pm_gone", None)])
            .chain([set(Scope::Session("no-such"), "@pm_t0", Some("x"))])
            .chain([set(
                Scope::Window(&format!("{session}:0")),
                "@pm_w",
                Some("w"),
            )])
            .collect();
        run(server.name(), &commands).unwrap();

        let names: Vec<String> = (0..tricky.len()).map(|i| format!("@pm_t{i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let options = read(server.name(), &names, &["@pm_w"], &[])
            .unwrap()
            .unwrap();
        let read_back = options
            .sessions
            .iter()
            .find(|s| s.target == session)
            .unwrap();
        let values: Vec<&str> = names.iter().map(|n| read_back.get(n)).collect();
        assert_eq!(values, tricky);
        let other = options
            .sessions
            .iter()
            .find(|s| s.target == prefixed)
            .unwrap();
        assert_eq!(other.get("@pm_t0"), "", "the exact session only");
        let window = options
            .windows
            .iter()
            .find(|w| w.target == format!("{session}:0"))
            .unwrap();
        assert_eq!(
            (window.session.as_str(), window.get("@pm_w")),
            (session.as_str(), "w")
        );
    }
}
