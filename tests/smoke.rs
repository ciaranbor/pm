//! Smoke tests: the real `pm` binary inside a `scripts/sandbox` (isolated
//! `$HOME`, private tmux server, harness shims on `PATH`). Each scenario
//! covers behaviour that depends on where a command is run from (cwd, the
//! real config dir, inherited env, inside its own tmux window) — nothing a
//! lib test can reach. Ignored by default:
//! `cargo test --test smoke -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use predicates::prelude::*;

const SANDBOX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/sandbox");

/// Scenarios each own the sandbox named after this pid, so they run one at
/// a time.
static LOCK: Mutex<()> = Mutex::new(());
static ATEXIT: Once = Once::new();
static RUN_COUNTER: AtomicU32 = AtomicU32::new(0);

const MAX_SYSTEM_PTYS: usize = 300;
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// One harness invocation as seen by the shim.
#[derive(Debug)]
struct Record {
    argv: Vec<String>,
    cwd: String,
    agent_name: String,
}

/// Result of a command run inside a tmux window. `exit: None, alive: false`
/// is the expected shape when the command kills its own window or session.
#[derive(Debug)]
struct Outcome {
    log: String,
    exit: Option<i32>,
    alive: bool,
}

struct Smoke {
    name: String,
    home: PathBuf,
    server: String,
    /// What tmux reports for the `/bin/sh` every window runs (`bash` on
    /// macOS), read from a probe window.
    shell: String,
    _guard: MutexGuard<'static, ()>,
}

impl Smoke {
    fn new() -> Self {
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enforce_system_pty_cap();

        let pid = std::process::id();
        let name = pid.to_string();
        // A previous scenario's sandbox, or a dead run's under a reused pid.
        sandbox_ok(&name, &["down"]);
        sandbox_ok(
            &name,
            &["up", "--pm", env!("CARGO_BIN_EXE_pm"), "--shell", "/bin/sh"],
        );
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

    fn home(&self) -> &Path {
        &self.home
    }

    fn proj(&self) -> PathBuf {
        self.home().join("proj")
    }

    /// Where the binary under test keeps the global registry for this HOME.
    fn projects_dir(&self) -> PathBuf {
        let config = if cfg!(target_os = "macos") {
            self.home().join("Library/Application Support")
        } else {
            self.home().join(".config")
        };
        config.join("pm").join("projects")
    }

    /// `scripts/sandbox run -C <cwd> -- <program>`: the sandbox's own env,
    /// nothing applied on this side.
    fn run_cmd(&self, cwd: &Path, program: &str) -> StdCommand {
        let mut cmd = StdCommand::new(SANDBOX);
        cmd.args(["run", "-n", &self.name, "-C"])
            .arg(cwd)
            .args(["--", program]);
        cmd
    }

    fn pm(&self, cwd: &Path) -> Command {
        Command::from_std(self.run_cmd(cwd, "pm"))
    }

    fn git(&self, cwd: &Path, args: &[&str]) {
        let out = self.run_cmd(cwd, "git").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Every tmux call goes through here so none can reach the default
    /// server (`-L` is always set).
    fn tmux(&self, args: &[&str]) -> std::process::Output {
        self.run_cmd(self.home(), "tmux")
            .args(["-L", &self.server])
            .args(args)
            .output()
            .expect("run tmux")
    }

    fn tmux_ok(&self, args: &[&str]) -> String {
        let out = self.tmux(args);
        assert!(
            out.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn sessions(&self) -> Vec<String> {
        let out = self.tmux(&["list-sessions", "-F", "#{session_name}"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn window_names(&self, session: &str) -> Vec<String> {
        let out = self.tmux(&["list-windows", "-t", session, "-F", "#{window_name}"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn find_window(&self, session: &str, name: &str) -> Option<String> {
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

    fn pane_command(&self, target: &str) -> String {
        self.tmux_ok(&["display", "-p", "-t", target, "#{pane_current_command}"])
    }

    fn target_alive(&self, target: &str) -> bool {
        self.tmux(&["list-panes", "-t", target]).status.success()
    }

    /// Run `cmd` in the shell of an existing window and wait for it to exit
    /// or for the window to disappear.
    fn run_in(&self, target: &str, cmd: &str) -> Outcome {
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
                    log: text,
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
    fn wait_for_shell(&self, target: &str) {
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
    fn argv_records(&self, agent: &str, n: usize) -> Vec<Record> {
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

    fn capture_all(&self) -> String {
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
    fn init_with_feature(&self) -> PathBuf {
        let proj = self.proj();
        self.pm(self.home())
            .args(["init", &proj.to_string_lossy()])
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
    fn set_agents_config(&self, table: &str, row: &str) {
        let config = self.proj().join(".pm/config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        let header = format!("[agents.{table}]\n");
        assert!(text.contains(&header), "{text}");
        std::fs::write(&config, text.replace(&header, &format!("{header}{row}\n"))).unwrap();
    }

    /// [`Self::init_with_feature`], then spawn `reviewer` in the feature
    /// with a globbable model id configured for it.
    fn init_with_spawned_reviewer(&self) -> PathBuf {
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
        sandbox_ok(&self.name, &["down"]);
    }
}

fn sandbox_ok(name: &str, args: &[&str]) -> String {
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
fn parse_record(text: &str) -> Option<Record> {
    let mut lines = text.lines();
    let argc: usize = lines.next()?.strip_prefix("argc=")?.parse().ok()?;
    let argv: Vec<String> = lines.by_ref().take(argc + 1).map(str::to_string).collect();
    if argv.len() != argc + 1 {
        return None;
    }
    let cwd = lines.next()?.strip_prefix("cwd=")?.to_string();
    let agent_name = lines.next()?.strip_prefix("PM_AGENT_NAME=")?.to_string();
    Some(Record {
        argv,
        cwd,
        agent_name,
    })
}

fn shell_quote(s: &str) -> String {
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

/// Catches: shell quoting of config-sourced values, the baseline path under
/// the real home, scope detection from cwd, and the dispatch seam itself.
#[test]
#[ignore]
fn spawn_builds_the_command_through_the_real_shell() {
    let s = Smoke::new();
    let login = s.init_with_spawned_reviewer();

    let records = s.argv_records("reviewer", 1);
    let rec = &records[0];
    let expected: Vec<String> = [
        s.home().join("bin/claude").to_string_lossy().to_string(),
        "--agent".into(),
        "reviewer".into(),
        "--model".into(),
        "x[1m]".into(),
        "--append-system-prompt-file".into(),
        s.home()
            .join(".agents/pm-baseline.md")
            .to_string_lossy()
            .to_string(),
        "Stand by.".into(),
    ]
    .into();
    assert_eq!(rec.argv, expected);
    assert_eq!(rec.agent_name, "reviewer");
    assert_eq!(Path::new(&rec.cwd), login);
}

/// Catches: the rename-then-kill ordering in `agent restart`, which only
/// matters when the caller's own process lives in the window being replaced.
#[test]
#[ignore]
fn restart_from_inside_the_agents_own_window() {
    let s = Smoke::new();
    let login = s.init_with_spawned_reviewer();
    s.argv_records("reviewer", 1);

    let old = s
        .find_window("proj/login", "reviewer")
        .expect("reviewer window");
    // Stop the shim so the window's shell (which still exports
    // PM_AGENT_NAME) takes commands again.
    s.tmux_ok(&["send-keys", "-t", &old, "C-c", ""]);
    s.wait_for_shell(&old);

    let outcome = s.run_in(&old, "pm agent restart reviewer");
    assert!(
        !outcome.alive,
        "old window survived the restart: {outcome:?}"
    );
    assert!(!outcome.log.contains("error:"), "{}", outcome.log);

    let names = s.window_names("proj/login");
    assert_eq!(
        names.iter().filter(|n| *n == "reviewer").count(),
        1,
        "windows: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "reviewer-restarting"),
        "windows: {names:?}"
    );
    let records = s.argv_records("reviewer", 2);
    assert_eq!(records.len(), 2);
    s.pm(&login)
        .args(["agent", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reviewer (active"));
}

/// Catches: `pm delete --force` run from the project's own main session,
/// which must remove every piece of state before the kill ends the caller.
#[test]
#[ignore]
fn delete_force_from_inside_main_session() {
    let s = Smoke::new();
    let proj = s.proj();
    let login = s.init_with_feature();
    std::fs::write(login.join("note.txt"), "smoke\n").unwrap();
    s.git(&login, &["add", "note.txt"]);
    s.git(&login, &["commit", "-q", "-m", "note"]);
    let registry_entry = s.projects_dir().join("proj.toml");
    assert!(registry_entry.exists(), "init should register the project");

    let outcome = s.run_in("proj/main:0", "pm delete --force --yes");
    assert_eq!(outcome.exit, None, "{outcome:?}");
    assert!(!outcome.alive);
    assert!(!outcome.log.contains("error:"), "{}", outcome.log);

    let sessions = s.sessions();
    assert!(
        !sessions.iter().any(|n| n.starts_with("proj/")),
        "sessions: {sessions:?}"
    );
    assert!(sessions.iter().any(|n| n == "keepalive"));
    assert!(!registry_entry.exists(), "registry entry left behind");
    assert!(!proj.exists(), "project root left behind");
}

/// Catches: `pm open` rebuilding sessions and respawning registered agents
/// from the registry alone, outside any tmux client.
#[test]
#[ignore]
fn open_restores_sessions_and_respawns_agents_from_the_registry() {
    let s = Smoke::new();
    s.init_with_spawned_reviewer();
    s.argv_records("reviewer", 1);
    let main = s.proj().join("main");

    s.pm(&main).arg("close").assert().success();
    assert!(!s.sessions().iter().any(|n| n.starts_with("proj/")));

    s.pm(&main)
        .arg("open")
        .assert()
        .success()
        .stdout(predicate::str::contains("Respawned 1 agents"));

    let sessions = s.sessions();
    assert!(sessions.iter().any(|n| n == "proj/main"), "{sessions:?}");
    assert!(sessions.iter().any(|n| n == "proj/login"), "{sessions:?}");
    let records = s.argv_records("reviewer", 2);
    assert_eq!(records.len(), 2);
}

/// Catches: the codex command line through the real shell and the directory
/// trust entry written under the real `~/.codex` (no `CODEX_HOME`), keyed by
/// the worktree path pm actually resolves.
#[test]
#[ignore]
fn codex_spawn_builds_the_command_and_trusts_the_worktree() {
    let s = Smoke::new();
    let login = s.init_with_feature();
    s.set_agents_config("harness", "\"*\" = \"codex\"");

    s.pm(&login)
        .args(["agent", "spawn", "reviewer"])
        .assert()
        .success();

    let records = s.argv_records("reviewer", 1);
    let rec = &records[0];
    let expected: Vec<String> = [
        s.home().join("bin/codex").to_string_lossy().to_string(),
        "-a".into(),
        "never".into(),
        "-s".into(),
        "danger-full-access".into(),
        "Stand by.".into(),
    ]
    .into();
    assert_eq!(rec.argv, expected);
    assert_eq!(rec.agent_name, "reviewer");
    assert_eq!(Path::new(&rec.cwd), login);

    let trust = std::fs::read_to_string(s.home().join(".codex/config.toml")).unwrap();
    assert!(
        trust.contains(&format!("[projects.\"{}\"]", login.display())),
        "{trust}"
    );
    assert!(trust.contains("trust_level = \"trusted\""), "{trust}");
}
