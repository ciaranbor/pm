//! The fixture smoke and real-harness tests share: a `scripts/sandbox` per
//! test binary (named after its pid, so scenarios in one binary run one at
//! a time), with helpers to drive `pm`, tmux and the sandbox's logs.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use predicates::prelude::*;

pub const SANDBOX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/sandbox");

/// Scenarios each own the sandbox named after this pid, so they run one at
/// a time.
static LOCK: Mutex<()> = Mutex::new(());
static ATEXIT: Once = Once::new();
static RUN_COUNTER: AtomicU32 = AtomicU32::new(0);

const MAX_SYSTEM_PTYS: usize = 300;
pub const WAIT: Duration = Duration::from_secs(30);
pub const POLL: Duration = Duration::from_millis(50);

/// One harness invocation as seen by the shim.
#[derive(Debug)]
pub struct Record {
    pub argv: Vec<String>,
    pub cwd: String,
    pub agent_name: String,
    /// For a claude `--resume`: `found` when the session was in the store
    /// as the shim started, else `missing`.
    pub resumed: Option<String>,
}

/// Result of a command run inside a tmux window. `exit: None, alive: false`
/// is the expected shape when the command kills its own window or session.
#[derive(Debug)]
pub struct Outcome {
    pub log: String,
    pub exit: Option<i32>,
    pub alive: bool,
}

pub struct Smoke {
    pub name: String,
    pub home: PathBuf,
    pub server: String,
    /// What tmux reports for the `/bin/sh` every window runs (`bash` on
    /// macOS), read from a probe window.
    pub shell: String,
    _guard: MutexGuard<'static, ()>,
}

impl Smoke {
    /// A sandbox with the harness shims.
    pub fn new() -> Self {
        Self::up(false)
    }

    /// A sandbox running the real harness binaries (`scripts/sandbox up
    /// --real`).
    pub fn real() -> Self {
        Self::up(true)
    }

    fn up(real: bool) -> Self {
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enforce_system_pty_cap();

        let pid = std::process::id();
        let name = pid.to_string();
        // A previous scenario's sandbox, or a dead run's under a reused pid.
        sandbox_ok(&name, &["down", "--force"]);
        let mut up = vec!["up", "--pm", env!("CARGO_BIN_EXE_pm"), "--shell", "/bin/sh"];
        if real {
            up.push("--real");
        }
        sandbox_ok(&name, &up);
        let home = PathBuf::from(sandbox_ok(&name, &["path"]));
        SANDBOX_HOME.set(home.clone()).ok();
        ATEXIT_PID.store(pid, Ordering::SeqCst);
        ATEXIT.call_once(|| {
            // Safety: `libc::atexit` is always safe to call; the handler only
            // spawns `tmux kill-server` and removes a directory.
            unsafe {
                libc::atexit(atexit_teardown);
            }
        });

        let mut smoke = Smoke {
            server: format!("pm-test-{name}"),
            name,
            home,
            shell: String::new(),
            _guard: guard,
        };
        // The shell is what the window's process reports once a command
        // has run in it (before that it may still be the login shell that
        // execs `default-command`).
        smoke.tmux_ok(&["new-window", "-d", "-t", "keepalive", "-n", "shell"]);
        let ready = smoke.home.join("log/shell-ready");
        smoke.tmux_ok(&[
            "send-keys",
            "-t",
            "keepalive:shell",
            &format!("touch {}", shell_quote(&ready.to_string_lossy())),
            "Enter",
        ]);
        let start = Instant::now();
        while !ready.exists() {
            assert!(start.elapsed() < WAIT, "probe shell never ran a command");
            std::thread::sleep(POLL);
        }
        smoke.shell = smoke.pane_command("keepalive:shell");
        smoke
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn proj(&self) -> PathBuf {
        self.home().join("proj")
    }

    /// Where the binary under test keeps the global registry for this HOME.
    pub fn projects_dir(&self) -> PathBuf {
        let config = if cfg!(target_os = "macos") {
            self.home().join("Library/Application Support")
        } else {
            self.home().join(".config")
        };
        config.join("pm").join("projects")
    }

    /// `scripts/sandbox run -C <cwd> -- <program>`: the sandbox's own env,
    /// nothing applied on this side.
    pub fn run_cmd(&self, cwd: &Path, program: &str) -> StdCommand {
        let mut cmd = StdCommand::new(SANDBOX);
        cmd.args(["run", "-n", &self.name, "-C"])
            .arg(cwd)
            .args(["--", program]);
        cmd
    }

    pub fn pm(&self, cwd: &Path) -> Command {
        Command::from_std(self.run_cmd(cwd, "pm"))
    }

    pub fn git(&self, cwd: &Path, args: &[&str]) {
        let out = self.run_cmd(cwd, "git").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Every tmux call goes through here so none can reach the default
    /// server (`-L` is always set).
    pub fn tmux(&self, args: &[&str]) -> std::process::Output {
        self.run_cmd(self.home(), "tmux")
            .args(["-L", &self.server])
            .args(args)
            .output()
            .expect("run tmux")
    }

    pub fn tmux_ok(&self, args: &[&str]) -> String {
        let out = self.tmux(args);
        assert!(
            out.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub fn sessions(&self) -> Vec<String> {
        let out = self.tmux(&["list-sessions", "-F", "#{session_name}"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn window_names(&self, session: &str) -> Vec<String> {
        let out = self.tmux(&["list-windows", "-t", session, "-F", "#{window_name}"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn find_window(&self, session: &str, name: &str) -> Option<String> {
        let out = self.tmux_ok(&[
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_name}\t#{session_name}:#{window_index}",
        ]);
        out.lines()
            .filter_map(|l| l.split_once('\t'))
            .find(|(n, _)| *n == name)
            .map(|(_, t)| t.to_string())
    }

    pub fn pane_command(&self, target: &str) -> String {
        self.tmux_ok(&["display", "-p", "-t", target, "#{pane_current_command}"])
    }

    pub fn target_alive(&self, target: &str) -> bool {
        self.tmux(&["list-panes", "-t", target]).status.success()
    }

    /// Run `cmd` in the shell of an existing window and wait for it to exit
    /// or for the window to disappear.
    pub fn run_in(&self, target: &str, cmd: &str) -> Outcome {
        let id = RUN_COUNTER.fetch_add(1, Ordering::SeqCst);
        let log = self.home().join("log").join(format!("run-{id}.log"));
        let log_q = shell_quote(&log.to_string_lossy());
        let line = format!(
            "sh -c {} > {log_q} 2>&1; echo __PM_EXIT=$? >> {log_q}",
            shell_quote(cmd)
        );
        self.tmux_ok(&["send-keys", "-t", target, &line, "Enter"]);

        let start = Instant::now();
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if let Some(code) = text
                .lines()
                .find_map(|l| l.strip_prefix("__PM_EXIT="))
                .and_then(|c| c.trim().parse().ok())
            {
                let log = text
                    .lines()
                    .filter(|l| !l.starts_with("__PM_EXIT="))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Outcome {
                    log,
                    exit: Some(code),
                    alive: true,
                };
            }
            if !self.target_alive(target) {
                return Outcome {
                    log: std::fs::read_to_string(&log).unwrap_or_default(),
                    exit: None,
                    alive: false,
                };
            }
            if start.elapsed() > WAIT {
                panic!(
                    "timed out waiting for `{cmd}` in {target}\nlog:\n{text}\n{}",
                    self.capture_all()
                );
            }
            std::thread::sleep(POLL);
        }
    }

    /// Wait until the window's shell is in the foreground again.
    pub fn wait_for_shell(&self, target: &str) {
        let start = Instant::now();
        loop {
            let cur = self.pane_command(target);
            if cur == self.shell {
                return;
            }
            assert!(
                start.elapsed() < WAIT,
                "{target} still running `{cur}`\n{}",
                self.capture_all()
            );
            std::thread::sleep(POLL);
        }
    }

    /// Shim records for `agent`, oldest first, once at least `n` exist.
    pub fn argv_records(&self, agent: &str, n: usize) -> Vec<Record> {
        let infix = format!("-{agent}-");
        let start = Instant::now();
        loop {
            let mut files: Vec<(std::time::SystemTime, PathBuf)> =
                std::fs::read_dir(self.home().join("log"))
                    .unwrap()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension().is_some_and(|x| x == "argv")
                            && p.file_name()
                                .and_then(|f| f.to_str())
                                .is_some_and(|f| f.contains(&infix))
                    })
                    .map(|p| (std::fs::metadata(&p).unwrap().modified().unwrap(), p))
                    .collect();
            files.sort();
            if files.len() >= n {
                let records: Vec<Record> = files
                    .iter()
                    .filter_map(|(_, p)| parse_record(&std::fs::read_to_string(p).unwrap()))
                    .collect();
                if records.len() >= n {
                    return records;
                }
            }
            assert!(
                start.elapsed() < WAIT,
                "expected {n} shim record(s) for '{agent}', found {}\n{}",
                files.len(),
                self.capture_all()
            );
            std::thread::sleep(POLL);
        }
    }

    pub fn capture_all(&self) -> String {
        let panes = self.tmux(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}:#{window_index} (#{window_name})",
        ]);
        let mut out = String::new();
        for line in String::from_utf8_lossy(&panes.stdout).lines() {
            let target = line.split(' ').next().unwrap_or(line);
            let text = self.tmux(&["capture-pane", "-p", "-J", "-t", target]);
            out.push_str(&format!(
                "--- {line}\n{}\n",
                String::from_utf8_lossy(&text.stdout)
            ));
        }
        out
    }

    /// `pm init` a project and `pm feat new login`. Returns the feature
    /// worktree path.
    pub fn init_with_feature(&self) -> PathBuf {
        let proj = self.proj();
        self.pm(self.home())
            .args(["init", "--no-main", &proj.to_string_lossy()])
            .assert()
            .success();
        let main = proj.join("main");
        self.pm(&main)
            .args(["feat", "new", "login"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Created feature 'login'"));
        proj.join("login")
    }

    /// Add a row to one of the empty `[agents.*]` tables `pm init` writes.
    pub fn set_agents_config(&self, table: &str, row: &str) {
        let config = self.proj().join(".pm/config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        let header = format!("[agents.{table}]\n");
        assert!(text.contains(&header), "{text}");
        std::fs::write(&config, text.replace(&header, &format!("{header}{row}\n"))).unwrap();
    }

    /// Append `toml` to the project's config.
    pub fn append_config(&self, toml: &str) {
        let config = self.proj().join(".pm/config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config, format!("{text}\n{toml}")).unwrap();
    }

    /// [`Self::init_with_feature`], then spawn `reviewer` in the feature
    /// with a globbable model id configured for it.
    pub fn init_with_spawned_reviewer(&self) -> PathBuf {
        let login = self.init_with_feature();
        self.set_agents_config("models", "reviewer = \"x[1m]\"");
        self.pm(&login)
            .args(["agent", "spawn", "reviewer"])
            .assert()
            .success();
        login
    }
}

impl Drop for Smoke {
    fn drop(&mut self) {
        sandbox_ok(&self.name, &["down", "--force"]);
    }
}

pub fn sandbox_ok(name: &str, args: &[&str]) -> String {
    let out = StdCommand::new(SANDBOX)
        .args(args)
        .args(["-n", name])
        .output()
        .expect("run scripts/sandbox");
    assert!(
        out.status.success(),
        "scripts/sandbox {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A record's argv never contains a newline in these scenarios, so a
/// line-per-item format with a leading count is enough.
pub fn parse_record(text: &str) -> Option<Record> {
    let mut lines = text.lines();
    let argc: usize = lines.next()?.strip_prefix("argc=")?.parse().ok()?;
    let argv: Vec<String> = lines.by_ref().take(argc + 1).map(str::to_string).collect();
    if argv.len() != argc + 1 {
        return None;
    }
    let cwd = lines.next()?.strip_prefix("cwd=")?.to_string();
    let agent_name = lines.next()?.strip_prefix("PM_AGENT_NAME=")?.to_string();
    let resumed = lines
        .next()
        .and_then(|l| l.strip_prefix("resumed="))
        .map(str::to_string);
    Some(Record {
        argv,
        cwd,
        agent_name,
        resumed,
    })
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn enforce_system_pty_cap() {
    let count = std::fs::read_dir("/dev")
        .map(|d| {
            d.flatten()
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("ttys"))
                })
                .count()
        })
        .unwrap_or(0);
    assert!(
        count < MAX_SYSTEM_PTYS,
        "system-wide pty count is {count} (threshold: {MAX_SYSTEM_PTYS}). \
         Check for leaked tmux sessions; kill test servers: \
         for s in /tmp/tmux-$(id -u)/pm-test-*; do tmux -L $(basename \"$s\") kill-server; done"
    );
}

static ATEXIT_PID: AtomicU32 = AtomicU32::new(0);
static SANDBOX_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Backstop for a run that dies without dropping its `Smoke`: the same
/// teardown `scripts/sandbox down` does, without depending on the script.
extern "C" fn atexit_teardown() {
    let pid = ATEXIT_PID.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    let _ = StdCommand::new("tmux")
        .args(["-L", &format!("pm-test-{pid}"), "kill-server"])
        .output();
    if let Some(home) = SANDBOX_HOME.get() {
        let _ = std::fs::remove_dir_all(home);
    }
}
