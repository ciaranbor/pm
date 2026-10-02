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
        sandbox_ok(&name, &["down", "--force"]);
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

    /// Append `toml` to the project's config.
    fn append_config(&self, toml: &str) {
        let config = self.proj().join(".pm/config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config, format!("{text}\n{toml}")).unwrap();
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
        sandbox_ok(&self.name, &["down", "--force"]);
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
        format!("--add-dir={}", s.proj().join(".pm/summaries").display()),
        "Stand by.".into(),
    ]
    .into();
    assert_eq!(rec.argv, expected);
    assert_eq!(rec.agent_name, "reviewer");
    assert_eq!(Path::new(&rec.cwd), login);
}

/// Catches: `agent spawn --scope` resolving the target from the caller's
/// cwd instead of the flag, run from `main` as an orchestrator would.
#[test]
#[ignore]
fn spawn_from_main_into_a_feature_with_scope() {
    let s = Smoke::new();
    let login = s.init_with_feature();
    s.pm(&s.proj().join("main"))
        .args(["agent", "spawn", "reviewer", "--scope", "login"])
        .assert()
        .success();

    let records = s.argv_records("reviewer", 1);
    assert_eq!(Path::new(&records[0].cwd), login);
    assert!(s.find_window("proj/login", "reviewer").is_some());
    assert!(s.find_window("proj/main", "reviewer").is_none());
}

/// Catches: `agent restart` run from one of the restarted agents' own
/// windows killing that window, and so itself, before the other agents
/// restart or anything is printed.
#[test]
#[ignore]
fn restart_from_inside_an_agents_own_window() {
    let s = Smoke::new();
    let login = s.init_with_spawned_reviewer();
    s.pm(&login)
        .args(["agent", "spawn", "helper", "--agent", "implementer"])
        .assert()
        .success();
    s.argv_records("reviewer", 1);
    s.argv_records("helper", 1);

    let old = s
        .find_window("proj/login", "reviewer")
        .expect("reviewer window");
    let old = s.tmux_ok(&["display", "-p", "-t", &old, "#{window_id}"]);
    // Stop the shim so the window's shell (which still exports
    // PM_AGENT_NAME) takes commands again.
    s.tmux_ok(&["send-keys", "-t", &old, "C-c", ""]);
    s.wait_for_shell(&old);

    let outcome = s.run_in(&old, "pm agent restart reviewer helper");
    assert!(
        !outcome.alive,
        "old window survived the restart: {outcome:?}"
    );
    for agent in ["reviewer", "helper"] {
        assert!(
            outcome.log.contains(&format!("Restarted agent '{agent}'")),
            "{}",
            outcome.log
        );
        assert_eq!(s.argv_records(agent, 2).len(), 2, "{agent}");
    }
    assert!(!outcome.log.contains("error:"), "{}", outcome.log);

    let names = s.window_names("proj/login");
    for agent in ["reviewer", "helper"] {
        assert_eq!(
            names.iter().filter(|n| *n == agent).count(),
            1,
            "windows: {names:?}"
        );
    }
    assert!(
        !names.iter().any(|n| n.ends_with("-restarting")),
        "windows: {names:?}"
    );
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
        "--no-daemon".into(),
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

/// Catches: opencode's env-prefixed command line through the real shell;
/// the session created before the window, from the real config dir, with
/// the caller's own agent identity and opencode config kept out of that
/// call and out of the window; `--standalone` on both invocations (the shim
/// refuses one without it); and the plugin installed under the real
/// `~/.config/opencode`.
#[test]
#[ignore]
fn opencode_spawn_creates_the_session_then_opens_it_standalone() {
    let s = Smoke::new();
    let login = s.init_with_feature();
    s.set_agents_config("harness", "\"*\" = \"opencode\"");
    s.set_agents_config("models", "reviewer = \"local/qwen[1m]\"");
    // As when an opencode agent spawns another: pm inherits the spawner's
    // identity and config, and so does every window of its session.
    let inherited = r#"{"model":"caller/model"}"#;
    for (key, value) in [
        ("OPENCODE_CONFIG", "/caller/opencode.json"),
        ("OPENCODE_CONFIG_CONTENT", inherited),
    ] {
        s.tmux(&["set-environment", "-t", "proj/login", key, value]);
    }

    s.pm(&login)
        .env("PM_AGENT_NAME", "main")
        .env("PM_OPENCODE_SESSION", "ses_of_main")
        .env("OPENCODE_CONFIG", "/caller/opencode.json")
        .env("OPENCODE_CONFIG_CONTENT", inherited)
        .args(["agent", "spawn", "reviewer"])
        .assert()
        .success();

    let api: Vec<PathBuf> = std::fs::read_dir(s.home().join("log"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "api"))
        .collect();
    assert_eq!(api.len(), 1, "{api:?}");
    let call = parse_record(&std::fs::read_to_string(&api[0]).unwrap()).unwrap();
    assert_eq!(
        call.argv[1..4],
        ["api", "--standalone", "session.create"],
        "{call:?}"
    );
    let body: serde_json::Value = serde_json::from_str(call.argv.last().unwrap()).unwrap();
    assert_eq!(body["agent"], "reviewer");
    assert_eq!(body["title"], "pm:reviewer");
    assert_eq!(
        body["model"],
        serde_json::json!({"providerID": "local", "id": "qwen[1m]"})
    );
    let api_text = std::fs::read_to_string(&api[0]).unwrap();
    assert!(
        api_text.ends_with("OPENCODE_CONFIG=\nOPENCODE_CONFIG_CONTENT=\n"),
        "the spawner's config reached opencode: {api_text}"
    );
    assert_eq!(
        Path::new(body["location"]["directory"].as_str().unwrap()),
        login.canonicalize().unwrap()
    );
    assert_eq!(
        call.agent_name, "",
        "the spawner's identity reached opencode"
    );
    let pid = api[0]
        .file_stem()
        .and_then(|f| f.to_str())
        .and_then(|f| f.rsplit('-').next())
        .unwrap();
    let session = format!("ses_shim{pid}");

    let records = s.argv_records("reviewer", 1);
    let rec = &records[0];
    let expected: Vec<String> = [
        s.home().join("bin/opencode").to_string_lossy().to_string(),
        "--standalone".into(),
        "--auto".into(),
        "--session".into(),
        session.clone(),
    ]
    .into();
    assert_eq!(rec.argv, expected);
    assert_eq!(rec.agent_name, "reviewer");
    assert_eq!(Path::new(&rec.cwd), login);

    let window = std::fs::read_dir(s.home().join("log"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|f| f.starts_with("opencode-reviewer-") && f.ends_with(".argv"))
        })
        .unwrap();
    let window = std::fs::read_to_string(window).unwrap();
    let config = window
        .lines()
        .find_map(|l| l.strip_prefix("OPENCODE_CONFIG="))
        .unwrap();
    assert_ne!(config, "/caller/opencode.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config).unwrap()).unwrap();
    assert_eq!(
        config,
        serde_json::json!({"model": "local/qwen[1m]", "enabled_providers": ["local"]})
    );
    assert!(window.ends_with("OPENCODE_CONFIG_CONTENT=\n"), "{window}");

    let registry = std::fs::read_to_string(s.proj().join(".pm/agents/login.toml")).unwrap();
    assert!(
        registry.contains(&format!("session_id = \"{session}\"")),
        "{registry}"
    );
    assert!(registry.contains("harness = \"opencode\""), "{registry}");

    let plugin = s.home().join(".config/opencode/plugins/pm-never-idle");
    for file in ["index.ts", "loop.ts", "pm.ts"] {
        assert!(plugin.join(file).is_file(), "{file}");
    }
}

/// Catches: an opencode call pm is waiting on outliving a Ctrl-C to pm. The
/// call runs in a process group of its own, which the terminal's SIGINT
/// does not reach, so pm must kill it — and whatever it started — itself.
#[test]
#[ignore]
fn interrupting_pm_kills_the_opencode_call_it_is_waiting_on() {
    use std::os::unix::process::ExitStatusExt;

    let s = Smoke::new();
    let login = s.init_with_feature();
    s.set_agents_config("harness", "reviewer = \"opencode\"");
    s.set_agents_config("models", "reviewer = \"local/qwen\"");
    let (call_pid, child_pid) = (s.home().join("call.pid"), s.home().join("child.pid"));
    let hang = s.home().join("hang");
    std::fs::write(
        &hang,
        format!(
            "#!/bin/sh\necho $$ > {}\nsleep 300 &\necho $! > {}\nwait\n",
            shell_quote(&call_pid.to_string_lossy()),
            shell_quote(&child_pid.to_string_lossy())
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hang, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    s.append_config(&format!(
        "[harness.opencode]\nbinary = {:?}\n",
        hang.to_string_lossy()
    ));

    // `scripts/sandbox run` execs, so the child is pm itself.
    let mut pm = s
        .run_cmd(&login, "pm")
        .args(["agent", "spawn", "reviewer"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !child_pid.exists() || std::fs::read_to_string(&child_pid).unwrap().is_empty() {
        assert!(start.elapsed() < WAIT, "pm never called opencode");
        std::thread::sleep(POLL);
    }
    // SAFETY: signals a process this test started.
    unsafe {
        libc::kill(pm.id() as libc::pid_t, libc::SIGINT);
    }
    assert_eq!(pm.wait().unwrap().signal(), Some(libc::SIGINT));

    for file in [&call_pid, &child_pid] {
        let pid: libc::pid_t = std::fs::read_to_string(file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let start = Instant::now();
        // SAFETY: signal 0 only probes for the process.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(start.elapsed() < WAIT, "{} outlived pm", file.display());
            std::thread::sleep(POLL);
        }
    }
}

/// Catches: the `opencode serve` a session migration runs its moves through
/// outliving a Ctrl-C to pm that lands mid-move.
#[test]
#[ignore]
fn interrupting_pm_mid_migration_kills_its_opencode_server() {
    use std::os::unix::process::ExitStatusExt;

    let s = Smoke::new();
    s.pm(s.home())
        .args(["init", &s.proj().to_string_lossy()])
        .assert()
        .success();
    let (server_pid, child_pid, moving) = (
        s.home().join("serve.pid"),
        s.home().join("serve-child.pid"),
        s.home().join("moving"),
    );
    let fake = s.home().join("fake-opencode");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             --version*) echo 2.0.18 ;;\n\
             serve*) echo $$ > {server}; echo 'server listening on http://127.0.0.1:9'\n\
             sleep 300 & echo $! > {child}; wait ;;\n\
             *session.list*) echo '{{\"data\":[{{\"id\":\"ses_a\"}}],\"cursor\":{{}}}}' ;;\n\
             *) touch {moving}; sleep 300 ;;\n\
             esac\n",
            server = shell_quote(&server_pid.to_string_lossy()),
            child = shell_quote(&child_pid.to_string_lossy()),
            moving = shell_quote(&moving.to_string_lossy()),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    s.append_config(&format!(
        "[harness.opencode]\nbinary = {:?}\n",
        fake.to_string_lossy()
    ));

    let mut pm = s
        .run_cmd(&s.proj().join("main"), "pm")
        .args([
            "harness",
            "migrate",
            "--harness",
            "opencode",
            "--from",
            "/gone/old",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let written = |file: &PathBuf| std::fs::read_to_string(file).is_ok_and(|t| !t.is_empty());
    while !moving.exists() || !written(&server_pid) || !written(&child_pid) {
        assert!(
            start.elapsed() < WAIT,
            "pm never asked opencode to move a session"
        );
        std::thread::sleep(POLL);
    }
    // SAFETY: signals a process this test started.
    unsafe {
        libc::kill(pm.id() as libc::pid_t, libc::SIGINT);
    }
    assert_eq!(pm.wait().unwrap().signal(), Some(libc::SIGINT));

    for file in [&server_pid, &child_pid] {
        let pid: libc::pid_t = std::fs::read_to_string(file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let start = Instant::now();
        // SAFETY: signal 0 only probes for the process.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(start.elapsed() < WAIT, "{} outlived pm", file.display());
            std::thread::sleep(POLL);
        }
    }
}

/// Catches: the binary probes resolving `claude` and `codex` through the
/// real `PATH` (lib tests skip them), and the refusal coming before the
/// worktree, branch and session rather than being rolled back after them.
#[test]
#[ignore]
fn feat_new_refuses_a_mixed_team_whose_harness_binaries_cannot_run() {
    use std::os::unix::fs::PermissionsExt;
    let s = Smoke::new();
    let proj = s.proj();
    s.pm(s.home())
        .args(["init", &proj.to_string_lossy()])
        .assert()
        .success();
    let main = proj.join("main");
    s.set_agents_config("harness", "reviewer = \"codex\"");
    s.append_config("[harness.codex]\nbypass_hook_trust = true\n");

    let shims: Vec<(PathBuf, Vec<u8>)> = ["claude", "codex"]
        .iter()
        .map(|name| s.home().join("bin").join(name))
        .map(|path| {
            let shim = std::fs::read(&path).unwrap();
            std::fs::write(&path, "#!/bin/sh\nexit 127\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            (path, shim)
        })
        .collect();

    s.pm(&main)
        .args(["feat", "new", "login", "--workflow", "implement-and-review"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "2 of 2 team member(s) cannot run on their harness",
        ))
        .stderr(predicate::str::contains(
            "implementer (claude-code): `claude` could not be run; install Claude Code",
        ))
        .stderr(predicate::str::contains(
            "reviewer (codex): `codex` could not be run; install codex",
        ));
    assert!(!proj.join("login").exists());
    assert!(!proj.join(".pm/features/login.toml").exists());
    assert!(!s.sessions().iter().any(|n| n == "proj/login"));

    for (path, shim) in shims {
        std::fs::write(path, shim).unwrap();
    }
    s.pm(&main)
        .args(["feat", "new", "login", "--workflow", "implement-and-review"])
        .assert()
        .success();
    assert_eq!(s.argv_records("reviewer", 1).len(), 1);
}

/// Catches: `pm doctor` probing the `claude` binary through the real `PATH`
/// only when an agent would launch on claude-code.
#[test]
#[ignore]
fn doctor_reports_an_unrunnable_claude_only_when_an_agent_is_on_it() {
    use std::os::unix::fs::PermissionsExt;
    let s = Smoke::new();
    let proj = s.proj();
    s.pm(s.home())
        .args(["init", &proj.to_string_lossy()])
        .assert()
        .success();
    let main = proj.join("main");
    let claude = s.home().join("bin/claude");
    std::fs::write(&claude, "#!/bin/sh\nexit 127\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let finding = "agents configured for claude-code cannot run: `claude` could not be run";

    s.pm(&main)
        .arg("doctor")
        .assert()
        .stdout(predicate::str::contains(finding));

    s.set_agents_config("harness", "\"*\" = \"codex\"");
    s.pm(&main)
        .arg("doctor")
        .assert()
        .stdout(predicate::str::contains(finding).not())
        .stdout(predicate::str::contains(
            "codex has not trusted pm's Stop hook",
        ));
}

/// Catches: the plugin line in a tmux config, `run-shell 'pm tmux init'`,
/// finding `pm` through the tmux server's own `PATH` and environment rather
/// than a shell's, for both init and the watcher it starts.
#[test]
#[ignore]
fn the_tmux_plugin_runs_from_the_servers_own_environment() {
    let s = Smoke::new();
    s.init_with_feature();
    s.pm(&s.proj().join("login"))
        .args(["feat", "status", "blocked", "-m", "which DB?"])
        .assert()
        .success();
    s.tmux_ok(&["set", "-g", "@pm-refresh-interval", "1"]);

    s.tmux_ok(&["run-shell", "pm tmux init"]);

    assert!(
        s.tmux_ok(&["show", "-gv", "window-status-format"])
            .contains("@pm_agent_badge")
    );
    let start = Instant::now();
    while s.tmux_ok(&["show", "-gqv", "@pm_count"]) != "1" {
        assert!(start.elapsed() < WAIT, "the watcher never refreshed");
        std::thread::sleep(POLL);
    }
    assert_eq!(
        s.tmux_ok(&["show", "-qv", "-t", "=proj/login:", "@pm_reason"]),
        "which DB?"
    );
}

/// Catches: a push missing the server the command runs against —
/// `PM_TMUX_SERVER` for the background refresh the Stop hook and `pm feat
/// status` start from an agent's pane, that and `$TMUX_PANE` for the hook's
/// own-window write — with no poll tick to hide it.
#[test]
#[ignore]
fn changes_made_from_an_agents_pane_reach_tmux_without_a_poll() {
    let s = Smoke::new();
    s.init_with_spawned_reviewer();
    s.argv_records("reviewer", 1);
    let window = s
        .find_window("proj/login", "reviewer")
        .expect("reviewer window");
    s.tmux_ok(&["send-keys", "-t", &window, "C-c", ""]);
    s.wait_for_shell(&window);
    s.tmux_ok(&["set", "-g", "@pm-refresh-interval", "3600"]);
    s.tmux_ok(&["run-shell", "pm tmux init"]);
    let until = |what: &str, done: &dyn Fn() -> bool| {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < WAIT, "{what}\n{}", s.capture_all());
            std::thread::sleep(POLL);
        }
    };
    let state = || s.tmux_ok(&["show", "-wqv", "-t", &window, "@pm_agent_state"]);
    let session = |name: &str| s.tmux_ok(&["show", "-qv", "-t", "=proj/login:", name]);
    until("the watcher's first refresh", &|| state() == "dead");
    assert_eq!(session("@pm_attention"), "dead");

    s.tmux_ok(&[
        "send-keys",
        "-t",
        &window,
        "PM_AGENT_NAME=reviewer pm harness hooks stop </dev/null >/dev/null 2>&1",
        "Enter",
    ]);
    until("the Stop hook's idle write", &|| state() == "idle");
    until("the Stop hook's push", &|| {
        session("@pm_attention") == "stalled"
    });
    s.tmux_ok(&["send-keys", "-t", &window, "C-c", ""]);
    s.wait_for_shell(&window);

    let outcome = s.run_in(
        &window,
        "PM_AGENT_NAME=reviewer pm feat status blocked -m 'which DB?'",
    );
    assert_eq!(outcome.exit, Some(0), "{}", outcome.log);
    until("the push from feat status", &|| {
        session("@pm_reason") == "which DB?"
    });
}
