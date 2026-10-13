//! `pm harness opencode-host`: what an opencode agent's window runs. It
//! owns the agent's `opencode serve --stdio` and runs the TUI attached to
//! it (`opencode --server <url>`), supervised as [`crate::harness::host`]
//! says.
//!
//! The server listens on TCP with a password the host picks, which only
//! the TUI's environment and the address file in the agent's runtime dir
//! hold: the server deletes it from its own environment before plugins and
//! tools run. The server and TUI are told where that file is
//! ([`SERVER_FILE_ENV`]) before either starts, so the session reports it as
//! its inbound channel whenever the plugin loads; a post reads the file,
//! which the host writes once the server listens and rewrites at a restart.
//!
//! The server's stdin is a pipe the host holds: the server exits when it
//! closes, so a host killed outright still ends its server. A server that
//! dies under a live TUI is started again on the same port with the same
//! password, where the TUI reconnects to it.
//!
//! The plugin, pm's config file and every hook run in the server, with the
//! window's environment, as they did in a `--standalone` TUI's own server.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::api::executable;
use super::{PASSWORD_ENV, SERVER_FILE_ENV, random_password};
use crate::error::{PmError, Result};
use crate::fs_utils::write_private;
use crate::harness::host::{Group, catch_signals, supervise};
use crate::state::{paths, runtime};

/// The server's URL and password, in the agent's runtime dir.
const ADDRESS_FILE: &str = "opencode-server.json";
/// Where the server's output goes, in its runtime dir.
const SERVER_LOG: &str = "opencode-server.log";
/// How long a server may take to report its URL.
const LISTEN_WITHIN: Duration = Duration::from_secs(30);

/// What an opencode agent's window runs, as pm resolved it at spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenCodeHost {
    pub binary: String,
    pub auto: bool,
    pub session: Option<String>,
}

/// Where `agent`'s server listens and the password it takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ServerAddress {
    pub(super) url: String,
    pub(super) password: String,
}

fn address_path(project_root: &Path, scope: &str, agent: &str) -> Result<PathBuf> {
    Ok(runtime::agent_dir(project_root, scope, agent)?.join(ADDRESS_FILE))
}

/// The server address recorded in `file`, while a host has one there.
pub(super) fn read_address(file: &Path) -> Option<ServerAddress> {
    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
}

/// Record `address` in `file`, readable by its owner only.
pub(super) fn write_address(file: &Path, address: &ServerAddress) -> Result<()> {
    write_private(file, serde_json::to_string(address)?.as_bytes())
}

impl OpenCodeHost {
    /// The server's arguments: a fresh port, or `port` again.
    fn server_args(port: Option<u16>) -> Vec<String> {
        let port = port.map_or("0".to_string(), |p| p.to_string());
        ["serve", "--stdio", "--port", &port]
            .map(str::to_string)
            .to_vec()
    }

    /// The TUI's arguments.
    fn tui_args(&self, url: &str) -> Vec<String> {
        let mut args = vec!["--server".to_string(), url.to_string()];
        if self.auto {
            args.push("--auto".to_string());
        }
        if let Some(session) = &self.session {
            args.extend(["--session".to_string(), session.clone()]);
        }
        args
    }
}

/// The agent's `opencode serve --stdio`, in a process group of its own.
struct Server {
    /// Dropped before the group, so the server sees its stdin close before
    /// it is signalled.
    _lease: ChildStdin,
    group: Group,
    url: String,
}

impl Server {
    /// Start the server, on `port` if given, and wait for its URL. Its
    /// output goes beside `address_file`.
    fn start(binary: &str, port: Option<u16>, password: &str, address_file: &Path) -> Result<Self> {
        let log_file = address_file.with_file_name(SERVER_LOG);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)?;
        let mut group = Group::spawn(
            Command::new(executable(binary))
                .args(OpenCodeHost::server_args(port))
                .env(PASSWORD_ENV, password)
                .env(SERVER_FILE_ENV, address_file)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(log),
        )
        .map_err(|e| PmError::Agent(format!("could not run `{binary} serve`: {e}")))?;
        let (Some(lease), Some(stdout)) = (group.0.stdin.take(), group.0.stdout.take()) else {
            return Err(PmError::Agent(
                "opencode serve started without its pipes".into(),
            ));
        };
        let (sender, announced) = mpsc::channel();
        std::thread::spawn(move || {
            // Read to the end: a server writing to a closed pipe dies.
            for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
                if let Some(url) = announced_url(&line) {
                    let _ = sender.send(url);
                }
            }
        });
        let url = announced.recv_timeout(LISTEN_WITHIN).map_err(|_| {
            PmError::Agent(format!(
                "opencode serve reported no address (its output: {})",
                log_file.display()
            ))
        })?;
        Ok(Self {
            _lease: lease,
            group,
            url,
        })
    }

    fn port(&self) -> Option<u16> {
        self.url
            .rsplit(':')
            .next()?
            .trim_end_matches('/')
            .parse()
            .ok()
    }
}

/// The URL in the line `serve --stdio` prints once it listens.
fn announced_url(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    Some(value.get("url")?.as_str()?.to_string())
}

/// Run the agent's server and TUI until the TUI exits or this process is
/// told to end. Returns the TUI's exit code.
pub fn run(host: &OpenCodeHost) -> Result<i32> {
    let agent = std::env::var("PM_AGENT_NAME")
        .ok()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| PmError::Agent("opencode-host runs only in a pm agent's window".into()))?;
    let (project_root, scope) = paths::agent_scope()?;
    let address_file = address_path(&project_root, &scope, &agent)?;
    let log = |line: &str| {
        runtime::log_stop_hook(
            &project_root,
            &scope,
            &agent,
            &format!("opencode-host: {line}"),
        )
    };
    catch_signals();
    serve(host, &address_file, &log)
}

/// [`run`], with the server's address recorded in `address_file`.
fn serve(host: &OpenCodeHost, address_file: &Path, log: &dyn Fn(&str)) -> Result<i32> {
    let password = random_password()?;
    let server = Server::start(&host.binary, None, &password, address_file)?;
    let url = server.url.clone();
    let port = server.port();
    let address = ServerAddress {
        url: url.clone(),
        password: password.clone(),
    };
    write_address(address_file, &address)?;
    let tui = Command::new(executable(&host.binary))
        .args(host.tui_args(&url))
        .env(PASSWORD_ENV, &password)
        .env(SERVER_FILE_ENV, address_file)
        .spawn()?;

    let restart = || {
        let server = Server::start(&host.binary, port, &password, address_file)?;
        if server.url != url {
            return Err(PmError::Agent(format!(
                "its server came back at {} instead of {url}, where the TUI looks for it",
                server.url
            )));
        }
        write_address(address_file, &address)?;
        Ok(server)
    };
    supervise(tui, server, |s: &mut Server| s.group.exited(), restart, log)
}

#[cfg(test)]
mod tests;
