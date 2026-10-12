//! `pm harness codex-host`: what a codex agent's window runs. It owns the
//! agent's `codex app-server`, runs the TUI against it, and applies pm's
//! permissions and launch prompt over the server's API.
//!
//! The server runs in a process group of its own, so the terminal's
//! signals reach only the TUI and this process, and is killed when the TUI
//! exits or this process is told to end (SIGHUP as the window closes,
//! SIGTERM, SIGINT). A server started in the background of a shell
//! instead outlives a killed window: the TUI holds the terminal's
//! foreground group, so the hangup never reaches it, and a loaded thread
//! would then take a wake as a turn nobody sees. A server that dies under
//! a live TUI is started again at the same socket, where the TUI
//! reconnects to it.
//!
//! Under `--remote` the TUI refuses permission flags on a resume or fork,
//! and `--add-dir` always, and on a fresh start it sends a sandbox derived
//! from its own config that drops a named permission profile. So the TUI
//! gets none: the launch prompt goes in by `turn/start` with pm's approval
//! policy and sandbox, which hold for that turn and every later one,
//! typed ones included. The server's own config carries the same settings
//! for anything that starts before it, and defines [`READ_ONLY_PROFILE`],
//! which `turn/start` can only name.
//!
//! A fresh TUI's thread is loaded before its first turn, so the launch
//! prompt goes to the thread the server holds once the TUI has connected;
//! a resumed one's, once the TUI has loaded it. The TUI makes a fork
//! itself (`fork <id>`): a thread the API forked stays loaded, so its
//! being loaded would not say the TUI has opened it, and a TUI resuming a
//! loaded thread drops the config it asks for, `--dangerously-bypass-hook-trust`
//! among them.
//!
//! On a remote resume the TUI reviews untrusted hooks whatever that flag
//! says (codex 0.160: it may be attaching to a thread already running), so
//! an agent whose hooks codex does not trust stops at that review on every
//! restart.

mod server;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use self::server::{Server, wait_within};
use super::app_server::{Client, Permissions, Sandbox};
use super::{BINARY, READ_ONLY, READ_ONLY_PROFILE, RESUME_IN_CURRENT_DIR, WORKSPACE_WRITE};
use crate::error::{PmError, Result};
use crate::state::{paths, runtime};

/// The socket an agent's app-server listens on, in its runtime dir.
const SOCKET_FILE: &str = "app-server.sock";
/// Where the server's output goes, in its runtime dir.
const SERVER_LOG: &str = "app-server.log";
/// How many times in a row a server that died under a live TUI is
/// started again; one that stays up for [`STAYED_UP`] resets the count.
const RESTARTS: u32 = 5;
const STAYED_UP: Duration = Duration::from_secs(60);
const TICK: Duration = Duration::from_millis(100);
/// How long one exchange with the server may take.
const CALL_LIMIT: Duration = Duration::from_secs(2);
/// How long the TUI may take to open its thread: past it, the launch
/// prompt is not given.
const OPEN_WITHIN: Duration = Duration::from_secs(600);

/// What a codex agent's window runs, as pm config resolved it at spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexHost {
    pub approval: String,
    pub sandbox: String,
    /// Directories outside the worktree the agent must be able to write.
    pub writable: Vec<PathBuf>,
    pub model: Option<String>,
    pub bypass_hook_trust: bool,
    /// The session to resume, or with `fork`, to fork.
    pub resume: Option<String>,
    pub fork: bool,
    pub prompt: Option<String>,
}

/// The socket `agent`'s app-server listens on.
pub(in crate::harness) fn socket_path(
    project_root: &Path,
    scope: &str,
    agent: &str,
) -> Result<PathBuf> {
    Ok(runtime::agent_dir(project_root, scope, agent)?.join(SOCKET_FILE))
}

impl CodexHost {
    /// The permissions `turn/start` applies.
    fn permissions(&self) -> Permissions {
        let sandbox = match self.sandbox.as_str() {
            READ_ONLY => Sandbox::Profile(READ_ONLY_PROFILE.to_string()),
            WORKSPACE_WRITE => Sandbox::WorkspaceWrite(self.writable.clone()),
            "danger-full-access" => Sandbox::FullAccess,
            other => Sandbox::Other(other.to_string()),
        };
        Permissions {
            approval: self.approval.clone(),
            sandbox,
        }
    }

    /// The server's arguments.
    fn server_args(&self, socket: &Path) -> Vec<String> {
        let mut args = vec![
            "app-server".to_string(),
            "--listen".to_string(),
            format!("unix://{}", socket.display()),
        ];
        let mut config = vec![format!("approval_policy={}", toml_string(&self.approval))];
        if self.sandbox == READ_ONLY {
            config.extend(super::read_only_profile(&self.writable));
        } else {
            config.push(format!("sandbox_mode={}", toml_string(&self.sandbox)));
        }
        if self.sandbox == WORKSPACE_WRITE {
            let roots: toml_edit::Array = self
                .writable
                .iter()
                .map(|dir| dir.to_string_lossy().into_owned())
                .collect();
            config.push(format!("sandbox_workspace_write.writable_roots={roots}"));
        }
        for kv in config {
            args.extend(["-c".to_string(), kv]);
        }
        args
    }

    /// The TUI's arguments.
    fn tui_args(&self, socket: &Path, worktree: &Path) -> Vec<String> {
        let mut args = vec![
            "--remote".to_string(),
            format!("unix://{}", socket.display()),
            "-C".to_string(),
            worktree.to_string_lossy().into_owned(),
        ];
        if let Some(model) = &self.model {
            args.extend(["-m".to_string(), model.clone()]);
        }
        if self.bypass_hook_trust {
            args.push("--dangerously-bypass-hook-trust".to_string());
        }
        if let Some(session) = &self.resume {
            args.extend([
                "-c".to_string(),
                RESUME_IN_CURRENT_DIR.to_string(),
                if self.fork { "fork" } else { "resume" }.to_string(),
                session.clone(),
            ]);
        }
        args
    }

    /// The thread the TUI opens, if known before it does: a resumed one.
    fn resumed(&self) -> Option<&str> {
        self.resume.as_deref().filter(|_| !self.fork)
    }
}

fn toml_string(s: &str) -> String {
    toml_edit::Value::from(s).to_string().trim().to_string()
}

/// Run the agent's server and TUI until the TUI exits or this process is
/// told to end. Returns the TUI's exit code.
pub fn run(host: &CodexHost) -> Result<i32> {
    let agent = std::env::var("PM_AGENT_NAME")
        .ok()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| PmError::Agent("codex-host runs only in a pm agent's window".into()))?;
    let (project_root, scope) = paths::agent_scope()?;
    let socket = socket_path(&project_root, &scope, &agent)?;
    let log_file = socket.with_file_name(SERVER_LOG);
    let worktree = std::env::current_dir()?.canonicalize()?;
    let log = {
        let (root, scope, agent) = (project_root.clone(), scope.clone(), agent.clone());
        move |line: &str| {
            runtime::log_stop_hook(&root, &scope, &agent, &format!("codex-host: {line}"))
        }
    };

    catch_signals();
    let mut server = Some(Server::start(host, &socket, &log_file)?);
    let mut tui = Command::new(BINARY)
        .args(host.tui_args(&socket, &worktree))
        .spawn()?;

    if let Some(prompt) = host.prompt.clone() {
        let (socket, permissions, log) = (socket.clone(), host.permissions(), log.clone());
        let resumed = host.resumed().map(str::to_string);
        std::thread::spawn(move || {
            if let Err(e) = launch_turn(&socket, resumed.as_deref(), &prompt, &permissions) {
                log(&format!("could not give the launch prompt: {e}"));
            }
        });
    }

    let mut restarts = 0;
    let mut started = Instant::now();
    loop {
        match tui.try_wait() {
            Ok(Some(status)) => return Ok(status.code().unwrap_or(1)),
            Ok(None) => {}
            Err(e) => {
                let _ = tui.kill();
                return Err(e.into());
            }
        }
        let signal = SIGNAL.swap(0, Ordering::SeqCst);
        if signal != 0 {
            drop(server);
            // SAFETY: signalling the child this process started.
            unsafe { libc::kill(tui.id() as libc::pid_t, signal) };
            let _ = wait_within(&mut tui, Duration::from_secs(5));
            let _ = tui.kill();
            let _ = tui.wait();
            return Ok(128 + signal);
        }
        let down = server.as_mut().is_none_or(Server::exited);
        if !down && started.elapsed() > STAYED_UP {
            restarts = 0;
        }
        if down && restarts < RESTARTS {
            restarts += 1;
            log("its app-server exited under a live TUI: starting it again");
            server = Server::start(host, &socket, &log_file)
                .inspect_err(|e| log(&e.to_string()))
                .ok();
            started = Instant::now();
        }
        std::thread::sleep(TICK);
    }
}

/// Give `prompt` to the TUI's thread — `resumed`, or the one a fresh or
/// forking TUI opens, the server's first — once the TUI has loaded it,
/// with `permissions`.
fn launch_turn(
    socket: &Path,
    resumed: Option<&str>,
    prompt: &str,
    permissions: &Permissions,
) -> Result<()> {
    let mut client = Client::connect(socket, CALL_LIMIT)?;
    let deadline = Instant::now() + OPEN_WITHIN;
    let thread = loop {
        if Instant::now() > deadline {
            return Err(PmError::Agent(format!(
                "the TUI opened no thread within {} s",
                OPEN_WITHIN.as_secs()
            )));
        }
        let loaded = client.loaded_threads()?;
        let found = match resumed {
            Some(id) => loaded.into_iter().find(|t| t == id),
            None => loaded.into_iter().next(),
        };
        if let Some(thread) = found {
            break thread;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    client.start_turn(&thread, prompt, Some(permissions))
}

static SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::SeqCst);
}

fn catch_signals() {
    for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler only stores to an atomic.
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeAppServer;
    use serde_json::json;

    fn host(sandbox: &str) -> CodexHost {
        CodexHost {
            approval: "never".into(),
            sandbox: sandbox.into(),
            writable: vec![PathBuf::from("/proj/.pm"), PathBuf::from("/proj/main/.git")],
            ..Default::default()
        }
    }

    fn config(args: &[String]) -> Vec<&str> {
        args.windows(2)
            .filter(|w| w[0] == "-c")
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn the_server_carries_the_sandbox_and_defines_the_read_only_profile() {
        let socket = Path::new("/r/app-server.sock");
        let args = host("read-only").server_args(socket);
        assert_eq!(
            args[..3],
            ["app-server", "--listen", "unix:///r/app-server.sock"]
        );
        assert_eq!(
            config(&args),
            [
                "approval_policy=\"never\"",
                "default_permissions=\"pm-read-only\"",
                "permissions.pm-read-only.filesystem={ \":root\" = \"read\", \
                 \"/proj/.pm\" = \"write\", \"/proj/main/.git\" = \"write\" }",
            ]
        );
        assert_eq!(
            config(&host("workspace-write").server_args(socket)),
            [
                "approval_policy=\"never\"",
                "sandbox_mode=\"workspace-write\"",
                "sandbox_workspace_write.writable_roots=[\"/proj/.pm\", \"/proj/main/.git\"]",
            ]
        );
    }

    #[test]
    fn the_tui_gets_no_permission_flags_and_resumes_in_the_worktree() {
        let socket = Path::new("/r/app-server.sock");
        let worktree = Path::new("/private/proj/main");
        let host = CodexHost {
            model: Some("gpt-5".into()),
            bypass_hook_trust: true,
            ..host("workspace-write")
        };
        let fresh = host.tui_args(socket, worktree);
        assert_eq!(
            fresh,
            [
                "--remote",
                "unix:///r/app-server.sock",
                "-C",
                "/private/proj/main",
                "-m",
                "gpt-5",
                "--dangerously-bypass-hook-trust",
            ]
        );
        let resumed = CodexHost {
            resume: Some("abc".into()),
            ..host.clone()
        };
        let args = resumed.tui_args(socket, worktree);
        assert_eq!(
            args[fresh.len()..],
            ["-c", "tui.resume_cwd=\"current\"", "resume", "abc"]
        );
        for refused in ["-a", "-s", "--add-dir"] {
            assert!(!args.iter().any(|a| a == refused), "{args:?}");
        }
        assert_eq!(resumed.resumed(), Some("abc"));

        let forked = CodexHost {
            fork: true,
            ..resumed
        };
        assert_eq!(
            forked.tui_args(socket, worktree)[fresh.len()..],
            ["-c", "tui.resume_cwd=\"current\"", "fork", "abc"]
        );
        assert_eq!(forked.resumed(), None);
    }

    #[test]
    fn each_sandbox_mode_becomes_the_turns_permissions() {
        assert_eq!(
            host("read-only").permissions().sandbox,
            Sandbox::Profile("pm-read-only".into())
        );
        assert_eq!(
            host("workspace-write").permissions().sandbox,
            Sandbox::WorkspaceWrite(host("x").writable)
        );
        assert_eq!(
            host("danger-full-access").permissions().sandbox,
            Sandbox::FullAccess
        );
    }

    #[test]
    fn the_launch_prompt_goes_to_the_thread_the_tui_opened_with_the_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let server = FakeAppServer::bind(dir.path());
        let permissions = host("danger-full-access").permissions();
        launch_turn(&server.path, None, "Stand by.", &permissions).unwrap();
        assert_eq!(
            server.turn().unwrap(),
            json!({
                "threadId": "t1",
                "input": [{"type": "text", "text": "Stand by."}],
                "approvalPolicy": "never",
                "sandboxPolicy": {"type": "dangerFullAccess"},
            })
        );
    }
}
