/// Test utilities shared across modules.
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU32, Ordering};

/// Process-wide lock guarding the global current working directory.
///
/// CWD is process-global, but `cargo test` runs tests in parallel within a
/// single process. Tests that mutate CWD via `std::env::set_current_dir`
/// take the WRITE lock; tests that merely *read* CWD take a READ lock. The
/// chief reader is `git clone`, which reads CWD at startup and aborts with
/// "this operation must be run in a work tree" if CWD has been moved into a
/// directory that isn't a work tree. This serialises the CWD mutator(s)
/// against the clone readers without flattening parallelism among the
/// readers themselves.
///
/// If you add a test that calls `std::env::set_current_dir`, take the write
/// lock here. If you add a test that runs `git clone` (or otherwise depends
/// on CWD), take the read lock.
pub static CWD_LOCK: RwLock<()> = RwLock::new(());

/// Serialises tests that write the test home's `.codex/config.toml` (codex
/// directory trust): the write is read-modify-write, so two concurrent
/// tests could each drop the other's entry.
pub static CODEX_CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The directory name Claude Code keeps the sessions of `path` under.
pub fn claude_key(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Write a stand-in `opencode` into `dir` and return its path, for
/// `[harness.opencode] binary`. It records its arguments in `<dir>/argv`,
/// one per line, and the opencode config its environment names in
/// `<dir>/env` (written first, so a new `argv` implies its `env`), keeping
/// earlier invocations as `argv.1`, `argv.2`, …; then
/// it prints `answer` and exits with `exit` — enough to play `opencode
/// api`, `opencode --version`, and the TUI a window launches.
pub fn fake_opencode(dir: &std::path::Path, answer: &str, exit: i32) -> String {
    fake_opencode_sequence(dir, &[answer], exit)
}

/// A [`fake_opencode`] whose n-th invocation in `dir` prints the n-th of
/// `answers`, and every later one the last.
pub fn fake_opencode_sequence(dir: &std::path::Path, answers: &[&str], exit: i32) -> String {
    let scripted: Vec<(&str, i32)> = answers.iter().map(|answer| (*answer, exit)).collect();
    fake_opencode_scripted(dir, &scripted)
}

/// A [`fake_opencode_sequence`] whose n-th invocation also exits with the
/// n-th code.
pub fn fake_opencode_scripted(dir: &std::path::Path, answers: &[(&str, i32)]) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("opencode");
    let answer_file = |name: &str| dir.join(format!("answer.{name}"));
    let exit_file = |name: &str| dir.join(format!("exit.{name}"));
    for stale in (1..).map(|n| n.to_string()) {
        let _ = std::fs::remove_file(exit_file(&stale));
        if std::fs::remove_file(answer_file(&stale)).is_err() {
            break;
        }
    }
    let (last, earlier) = answers.split_last().expect("at least one answer");
    let write = |name: &str, (answer, exit): &(&str, i32)| {
        std::fs::write(answer_file(name), format!("{answer}\n"))
            .expect("write fake opencode answer");
        std::fs::write(exit_file(name), exit.to_string()).expect("write fake opencode exit");
    };
    for (n, answer) in earlier.iter().enumerate() {
        write(&(n + 1).to_string(), answer);
    }
    write("last", last);
    let script = format!(
        "#!/bin/sh\nn=1; while [ -e '{log}'.$n ]; do n=$((n+1)); done\n\
         k=$n; [ -e '{log}' ] && mv '{log}' '{log}'.$n && k=$((n+1))\n\
         printf 'OPENCODE_CONFIG=%s\\nOPENCODE_CONFIG_CONTENT=%s\\n' \\\n\
         \"$OPENCODE_CONFIG\" \"$OPENCODE_CONFIG_CONTENT\" > '{env}.tmp' && mv '{env}.tmp' '{env}'\n\
         printf '%s\\n' \"$@\" > '{log}.tmp' && mv '{log}.tmp' '{log}'\n\
         a='{answer}'.$k; e='{exit}'.$k; [ -e \"$a\" ] || {{ a='{answer}'.last; e='{exit}'.last; }}\n\
         cat \"$a\"\nexit \"$(cat \"$e\")\"\n",
        log = dir.join("argv").display(),
        env = dir.join("env").display(),
        answer = dir.join("answer").display(),
        exit = dir.join("exit").display(),
    );
    std::fs::write(&bin, script).expect("write fake opencode");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake opencode");
    bin.to_string_lossy().into_owned()
}

/// What the last [`fake_opencode`] invocation in `dir` was called with;
/// empty when it has not run.
pub fn fake_opencode_argv(dir: &std::path::Path) -> Vec<String> {
    read_lines(&dir.join("argv"))
}

/// Every [`fake_opencode`] invocation in `dir`, oldest first.
pub fn fake_opencode_calls(dir: &std::path::Path) -> Vec<Vec<String>> {
    let mut calls: Vec<Vec<String>> = (1..)
        .map(|n| dir.join(format!("argv.{n}")))
        .take_while(|path| path.exists())
        .map(|path| read_lines(&path))
        .collect();
    calls.extend(Some(fake_opencode_argv(dir)).filter(|last| !last.is_empty()));
    calls
}

fn read_lines(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// A `claude` that is `sleep` under another name, in the test home: a
/// window running it has a Claude Code harness as far as pm can tell.
fn fake_claude() -> std::path::PathBuf {
    static FAKE: OnceLock<std::path::PathBuf> = OnceLock::new();
    FAKE.get_or_init(|| {
        let dir = test_home().join("fake-bin");
        std::fs::create_dir_all(&dir).expect("create fake-bin");
        let bin = dir.join("claude");
        let _ = std::os::unix::fs::symlink("/bin/sleep", &bin);
        bin
    })
    .clone()
}

static TMUX_SERVER_COUNTER: AtomicU32 = AtomicU32::new(0);
static SHARED_SERVER_NAME: OnceLock<String> = OnceLock::new();
static TEST_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();

/// PID whose `pm-test-<pid>` server should be killed by the atexit handler.
/// Stored separately because `extern "C" fn` cannot capture state.
static ATEXIT_PID: AtomicU32 = AtomicU32::new(0);

/// Hard ceiling on concurrent live sessions in the shared test server.
/// Exceeding this indicates a leak — the test that trips it panics with a
/// recovery command instead of silently exhausting the system pty budget.
const MAX_TEST_SESSIONS: usize = 200;

/// System-wide pty ceiling. If the total number of allocated ptys on the
/// system reaches this threshold, tests abort before creating more. The
/// macOS hard limit is 511; this leaves headroom for the user's own
/// sessions and agents.
const MAX_SYSTEM_PTYS: usize = 300;

/// Count system-wide allocated ptys by reading `/dev/ttys*` entries.
/// Returns `None` if the count cannot be determined.
fn system_pty_count() -> Option<usize> {
    let entries = std::fs::read_dir("/dev").ok()?;
    let count = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("ttys"))
        })
        .count();
    Some(count)
}

/// Check the system-wide pty count and return an error message if it
/// exceeds the safety threshold.
fn enforce_system_pty_cap() -> Result<(), String> {
    if let Some(count) = system_pty_count()
        && count >= MAX_SYSTEM_PTYS
    {
        return Err(format!(
            "system-wide pty count is {count} (threshold: {MAX_SYSTEM_PTYS}, macOS limit: 511). \
                 Aborting test to prevent pty exhaustion. \
                 Check for leaked tmux sessions: tmux list-sessions; \
                 kill test servers: for s in /tmp/tmux-$(id -u)/pm-test-*; do tmux -L $(basename \"$s\") kill-server; done"
        ));
    }
    Ok(())
}

/// Directory tmux uses for its unix sockets: `tmux-<uid>` under
/// `TMUX_TMPDIR` (matching tmux itself), or under `/tmp`.
fn tmux_socket_dir() -> std::path::PathBuf {
    let base = std::env::var("TMUX_TMPDIR")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/tmp".to_string());
    // Safety: getuid is always safe to call.
    let uid = unsafe { libc::getuid() };
    std::path::PathBuf::from(base).join(format!("tmux-{uid}"))
}

/// Check whether a pid refers to a live process. Uses `kill(pid, 0)` which
/// returns 0 for live processes and sets errno to ESRCH for dead ones.
fn pid_is_alive(pid: u32) -> bool {
    // Safety: kill with sig=0 performs permission/existence check only.
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM means a live process we don't own — still alive. Only ESRCH
    // indicates the pid is truly gone.
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Kill the tmux server listening on `socket`, if any. `-S` rather than
/// `-L` so the reaper can act on a socket dir other than tmux's default.
fn kill_server_at(socket: &std::path::Path) {
    let _ = std::process::Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .arg("kill-server")
        .output();
}

/// Enumerate `pm-test-<pid>` sockets in `dir` and kill any whose pid no
/// longer refers to a live process. Ignores the current process's own
/// socket and malformed filenames.
fn reap_dead_test_servers(dir: &std::path::Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let self_pid = std::process::id();
    for entry in entries.flatten() {
        let fname = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let pid_str = match fname.strip_prefix("pm-test-") {
            Some(s) => s,
            None => continue,
        };
        // `pm-test-<pid>`, or `pm-test-<pid>-<suffix>` for a test's own server.
        let pid: u32 = match pid_str.split('-').next().unwrap_or_default().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        // Skip our own pid: `pid_is_alive(self_pid)` is always true, and
        // the stale-self-pid case (pid reuse across test binaries) is
        // handled in `shared_server_name()` at init time, before we create
        // our own server. Stripping it here also keeps the reaper safe to
        // call after init without risking killing our live server.
        if pid == self_pid {
            continue;
        }
        if pid_is_alive(pid) {
            continue;
        }
        // `kill-server` does NOT remove the socket when the server is
        // already dead, so it is unlinked explicitly.
        kill_server_at(&entry.path());
        let _ = std::fs::remove_file(entry.path());
    }
}

/// Check the soft cap on live sessions. Returns `Err(message)` when the
/// caller should panic; the message is the exact recovery hint shown to
/// the user. Pure function so it can be unit-tested directly.
fn enforce_soft_cap(count: usize, pid: u32) -> Result<(), String> {
    if count > MAX_TEST_SESSIONS {
        Err(format!(
            "pty budget exceeded ({count} sessions in pm-test-{pid}). \
             This usually indicates leaked test sessions. \
             Recover with: tmux -L pm-test-{pid} kill-server"
        ))
    } else {
        Ok(())
    }
}

/// atexit handler: kill the shared test server owned by this process.
/// Registered once via `libc::atexit` after the server is created. Runs on
/// normal exit and after panic unwind; does NOT run on SIGKILL or abort()
/// — those are cleaned up by the startup reaper on the next run.
///
/// This runs after `main` returns while the C runtime and libstd are still
/// usable, so spawning a subprocess via `Command` is fine. It is NOT a
/// signal handler, so async-signal-safety rules do not apply.
extern "C" fn atexit_kill_shared_server() {
    let pid = ATEXIT_PID.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    let name = format!("pm-test-{pid}");
    let _ = crate::tmux::kill_server(Some(&name));
}

const TEST_HOME_PREFIX: &str = "pm-test-home-";

/// The shell every window of the shared test server runs.
const HERMETIC_SHELL: &str = "/bin/sh";

/// Remove `pm-test-home-<pid>` dirs left by test binaries that died without
/// running their atexit handler.
fn reap_dead_test_homes() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let self_pid = std::process::id();
    for entry in entries.flatten() {
        let Ok(fname) = entry.file_name().into_string() else {
            continue;
        };
        let Some(pid) = fname
            .strip_prefix(TEST_HOME_PREFIX)
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == self_pid || pid_is_alive(pid) {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

extern "C" fn atexit_remove_test_home() {
    if let Some(home) = TEST_HOME.get() {
        let _ = std::fs::remove_dir_all(home);
    }
}

/// The `$HOME` stand-in every global-tier path uses under `cfg(test)`: one
/// `pm-test-home-<pid>` temp dir per test binary, shared by all its tests.
/// The global asset tier is installed in it before any test sees it: a
/// first install writes temp files into directories a concurrent install is
/// listing and copying, while a later one finds everything up to date and
/// writes nothing. Tests may read the tier freely; a test that needs to
/// *mutate* it must use the explicit-dir variants against its own tempdir.
pub fn test_home() -> &'static std::path::Path {
    TEST_HOME.get_or_init(|| {
        reap_dead_test_homes();
        let dir = std::env::temp_dir().join(format!("{TEST_HOME_PREFIX}{}", std::process::id()));
        // Pid reuse: a stale dir under our own pid holds another run's state.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test home");
        // Safety: `libc::atexit` is always safe to call; the handler only
        // removes a directory.
        unsafe {
            libc::atexit(atexit_remove_test_home);
        }
        crate::commands::skills::install_global_in(&crate::commands::skills::GlobalStore::at(&dir))
            .expect("install the global tier into the test home");
        dir
    })
}

/// Start a tmux server on socket `name` for tests to type commands into.
///
/// Under the developer's tmux config and interactive shell, a window's rc
/// files can take over 10s to load under a loaded suite, and can drop keys
/// typed before the prompt; so the server reads no config and every window
/// runs `sh` with no startup files. tmux starts a window as
/// `$default-shell -c <default-command>`, and takes `default-shell` from
/// `SHELL`, so both are set; with `ENV` unset the interactive `sh` reads
/// nothing. The keepalive session keeps the server up: without it the
/// server shuts down each time a test cleans up its sessions.
fn start_hermetic_server(name: &str) -> bool {
    // A socket left under `name` by a dead run whose pid ours reuses would
    // stop tmux from starting a server on it.
    let _ = crate::tmux::kill_server(Some(name));
    let _ = std::fs::remove_file(tmux_socket_dir().join(name));
    std::process::Command::new("tmux")
        .env("SHELL", HERMETIC_SHELL)
        .env_remove("ENV")
        .args(["-L", name, "-f", "/dev/null"])
        .args([
            "new-session",
            "-d",
            "-s",
            "keepalive",
            "-c",
            "/tmp",
            HERMETIC_SHELL,
        ])
        .args([";", "set-option", "-g", "default-shell", HERMETIC_SHELL])
        .args([";", "set-option", "-g", "default-command", HERMETIC_SHELL])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A tmux server of one test's own, `pm-test-<pid>-<suffix>`, killed on
/// drop (and reaped with the shared server's if the run dies). For a test
/// that attaches a client: on the shared server it would be the client
/// other tests' `switch-client` and `#{client_session}` find.
pub struct OwnServer(String);

impl OwnServer {
    pub fn start(suffix: &str) -> Self {
        if let Err(msg) = enforce_system_pty_cap() {
            panic!("{msg}");
        }
        let name = format!("pm-test-{}-{suffix}", std::process::id());
        assert!(start_hermetic_server(&name), "start tmux server {name}");
        Self(name)
    }

    pub fn name(&self) -> Option<&str> {
        Some(&self.0)
    }
}

impl Drop for OwnServer {
    fn drop(&mut self) {
        let _ = crate::tmux::kill_server(self.name());
    }
}

/// Whether a server listens, or once listened, on socket `name`.
pub fn server_socket_exists(name: &str) -> bool {
    tmux_socket_dir().join(name).exists()
}

fn shared_server_name() -> &'static str {
    SHARED_SERVER_NAME.get_or_init(|| {
        // Reap any `pm-test-<pid>` servers left behind by dead test binaries
        // (SIGKILL, abort, or anything else that bypassed our atexit).
        reap_dead_test_servers(&tmux_socket_dir());

        let pid = std::process::id();
        let name = format!("pm-test-{pid}");

        start_hermetic_server(&name);

        // Register the atexit cleanup exactly once. Store the pid first
        // because the extern "C" fn cannot capture.
        ATEXIT_PID.store(pid, Ordering::SeqCst);
        // Safety: `libc::atexit` is always safe to call. The handler runs
        // after `main` returns while libstd is still usable (it is not a
        // signal handler), so invoking `Command` from inside it is fine.
        unsafe {
            libc::atexit(atexit_kill_shared_server);
        }
        name
    })
}

/// RAII guard for a shared tmux test server. All tests share a single tmux
/// server process (one per cargo-test binary) to minimise pty usage. Each
/// `TestServer` instance gets a unique prefix so session names don't collide
/// across parallel tests. On drop, only sessions belonging to this instance
/// are killed — the shared server stays alive for other tests.
pub struct TestServer {
    prefix: String,
}

impl Default for TestServer {
    fn default() -> Self {
        Self::new()
    }
}

impl TestServer {
    pub fn new() -> Self {
        // System-wide pty check: abort before creating any new sessions
        // if we're approaching the macOS pty limit.
        if let Err(msg) = enforce_system_pty_cap() {
            panic!("{msg}");
        }

        let id = TMUX_SERVER_COUNTER.fetch_add(1, Ordering::SeqCst);
        let server = Self {
            prefix: format!("t{id}"),
        };

        // Soft cap: if the shared server is holding an unreasonable number
        // of sessions we've almost certainly leaked. Fail loudly with a
        // recovery command rather than cascading into system-wide pty
        // exhaustion.
        let count = crate::tmux::list_sessions(server.name())
            .map(|s| s.len())
            .unwrap_or(0);
        if let Err(msg) = enforce_soft_cap(count, std::process::id()) {
            panic!("{msg}");
        }

        server
    }

    /// The registry dir `setup_project` wrote for a project at `project_path`:
    /// a sibling `registry/` directory.
    pub fn registry_dir(project_path: &std::path::Path) -> std::path::PathBuf {
        project_path.parent().unwrap().join("registry")
    }

    /// Get the shared server name to pass to tmux functions as `Some(&str)`.
    pub fn name(&self) -> Option<&str> {
        Some(shared_server_name())
    }

    /// Return a name scoped to this test instance. Use this for project names
    /// and direct session names to avoid collisions with other parallel tests.
    pub fn scope(&self, name: &str) -> String {
        format!("{}-{}", self.prefix, name)
    }

    /// Create a project with init, returning `(project_path, projects_dir, project_name)`.
    pub fn setup_project(
        &self,
        dir: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let name = self.scope("myapp");
        let project_path = dir.join(&name);
        let projects_dir = dir.join("registry");
        crate::commands::init::init(&project_path, &projects_dir, None, self.name()).unwrap();
        (project_path, projects_dir, name)
    }

    /// `setup_project` with the main branch renamed to `master` and recorded
    /// as such, so a test cannot pass by assuming `main`.
    pub fn setup_master_project(
        &self,
        dir: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let (project_path, projects_dir, project_name) = self.setup_project(dir);
        let main = crate::state::paths::main_worktree(&project_path);
        crate::git::rename_branch(&main, "main", "master").unwrap();
        let mut entry =
            crate::state::project::ProjectEntry::load(&projects_dir, &project_name).unwrap();
        entry.main_branch = "master".to_string();
        entry.save(&projects_dir, &project_name).unwrap();
        (project_path, projects_dir, project_name)
    }

    /// A `master`-default project holding a `child` feature stacked on a
    /// `parent` feature that has since been merged and cleaned up, so
    /// `child`'s base branch no longer exists. Each feature has one commit.
    pub fn setup_orphaned_child(
        &self,
        dir: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        use crate::commands::{feat_merge, feat_new};
        let (project_path, projects_dir, _) = self.setup_master_project(dir);
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "parent",
            self.name(),
        ))
        .unwrap();
        Self::add_feature_commit(&project_path, "parent");
        feat_new::feat_new(&feat_new::FeatNewParams {
            project_root: &project_path,
            projects_dir: &projects_dir,
            name: "child",
            name_override: None,
            context: None,
            base: Some("parent"),
            workflow: None,
            tmux_server: self.name(),
        })
        .unwrap();
        let child = project_path.join("child");
        std::fs::write(child.join("child.txt"), "child work").unwrap();
        crate::git::stage_file(&child, "child.txt").unwrap();
        crate::git::commit(&child, "child work").unwrap();
        feat_merge::feat_merge(&project_path, &projects_dir, "parent", false, self.name()).unwrap();
        assert!(
            !crate::git::branch_exists(
                &crate::state::paths::main_worktree(&project_path),
                "parent"
            )
            .unwrap()
        );
        (project_path, projects_dir)
    }

    /// Create a project and a feature, returning `(project_path, project_name)`.
    pub fn setup_project_with_feature(
        &self,
        dir: &std::path::Path,
        feature_name: &str,
    ) -> (std::path::PathBuf, String) {
        let (project_path, projects_dir, project_name) = self.setup_project(dir);
        crate::commands::feat_new::feat_new(
            &crate::commands::feat_new::FeatNewParams::with_defaults(
                &project_path,
                &projects_dir,
                feature_name,
                self.name(),
            ),
        )
        .unwrap();
        (project_path, project_name)
    }

    /// Create a project without tmux sessions.
    ///
    /// Replicates the filesystem structure of `pm init` (git repo, `.pm/`
    /// directory, config, hooks, global asset tier, registry entry) but skips
    /// creating the tmux session. Use this for tests that only need the project
    /// directory layout and never interact with tmux.
    ///
    /// Keep in sync with `commands::init::init()` — if init gains new
    /// setup steps, they should be mirrored here.
    pub fn setup_project_no_tmux(
        &self,
        dir: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        use crate::state::paths;
        let name = self.scope("myapp");
        let project_path = dir.join(&name);
        let projects_dir = dir.join("registry");

        // Replicate init's filesystem work without tmux
        std::fs::create_dir_all(&project_path).unwrap();
        let main_path = paths::main_worktree(&project_path);
        crate::git::init_repo(&main_path).unwrap();

        let pm_dir = paths::pm_dir(&project_path);
        let features_dir = paths::features_dir(&project_path);
        std::fs::create_dir_all(&features_dir).unwrap();

        // Write project config
        use crate::state::project::{AgentsConfig, ProjectConfig, ProjectInfo};
        let config = ProjectConfig {
            project: ProjectInfo {
                name: name.clone(),
                max_features: None,
            },
            agents: AgentsConfig::default(),
            harness: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        crate::hooks::bootstrap(&project_path).unwrap();
        crate::commands::docs::bootstrap(&project_path).unwrap();
        crate::commands::state_cmd::init(&project_path).unwrap();
        crate::commands::hooks_install::install(Some(&project_path)).unwrap();
        crate::commands::skills::install_global().unwrap();
        crate::commands::skills::write_migration_marker(&project_path).unwrap();

        // Register in global registry
        use crate::state::project::ProjectEntry;
        let entry = ProjectEntry {
            root: crate::path_utils::to_portable(&project_path),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, &name).unwrap();

        (project_path, projects_dir, name)
    }

    /// Create a project and a feature without tmux sessions.
    ///
    /// Same as `setup_project_with_feature` but skips all tmux interaction.
    /// The git branch and worktree are created, and feature state is written,
    /// but no tmux sessions exist for the project or feature.
    pub fn setup_project_with_feature_no_tmux(
        &self,
        dir: &std::path::Path,
        feature_name: &str,
    ) -> (std::path::PathBuf, String) {
        use crate::state::paths;
        let (project_path, _, project_name) = self.setup_project_no_tmux(dir);
        let main_worktree = paths::main_worktree(&project_path);
        let worktree_path = project_path.join(feature_name);

        // Create git branch + worktree
        crate::git::create_branch_from(&main_worktree, feature_name, "main").unwrap();
        crate::git::add_worktree(&main_worktree, &worktree_path, feature_name).unwrap();

        // Write feature state
        let features_dir = paths::features_dir(&project_path);
        use crate::state::feature::{FeatureState, FeatureStatus};
        let now = chrono::Utc::now();
        let state = FeatureState {
            branch: feature_name.to_string(),
            worktree: feature_name.to_string(),
            status: FeatureStatus::Wip,
            pr: String::new(),
            base: "main".to_string(),
            context: String::new(),
            workflow: None,
            created: now,
            last_active: now,
            progress: Default::default(),
            blocked_reason: None,
            blocked_by: None,
        };
        state.save(&features_dir, feature_name).unwrap();

        crate::commands::seed::seed_feature_assets(&project_path, &worktree_path).unwrap();

        (project_path, project_name)
    }

    /// Poll a window's scrollback until `needle` appears (the shell echoes
    /// typed commands, so this observes what a spawn actually launched).
    /// Panics with the captured text on timeout.
    /// Set a variable every new window of `session` inherits, as when the
    /// session was created from a shell that had it.
    pub fn set_session_env(&self, session: &str, key: &str, value: &str) {
        let mut command = std::process::Command::new("tmux");
        if let Some(name) = self.name() {
            command.args(["-L", name]);
        }
        let status = command
            .args(["set-environment", "-t", session, key, value])
            .status()
            .expect("run tmux");
        assert!(status.success(), "tmux set-environment {key}");
    }

    /// The id (`%N`) of the pane `target` names; a window names its active
    /// pane.
    pub fn pane_id(&self, target: &str) -> String {
        self.tmux_stdout(&["display-message", "-p", "-t", target, "#{pane_id}"])
    }

    /// Split a new pane, running a shell, in front of `window`'s first pane,
    /// so it becomes pane 0 and the active pane, as a user splitting an
    /// agent's window might. Returns its id.
    pub fn split_before(&self, window: &str) -> String {
        let first = format!("{window}.0");
        self.tmux_stdout(&["split-window", "-b", "-P", "-F", "#{pane_id}", "-t", &first])
    }

    pub fn tmux_stdout(&self, args: &[&str]) -> String {
        let output = std::process::Command::new("tmux")
            .args(["-L", self.name().unwrap()])
            .args(args)
            .output()
            .expect("run tmux");
        assert!(output.status.success(), "tmux {args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    pub fn wait_for_pane_text(&self, target: &str, needle: &str) {
        let mut last = String::new();
        for _ in 0..500 {
            if let Ok(text) = crate::tmux::capture_pane(self.name(), target) {
                if text.contains(needle) {
                    return;
                }
                last = text;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "wait_for_pane_text: '{needle}' never appeared in window '{target}'; last capture:\n{last}"
        );
    }

    /// Create a tmux window running a stand-in `claude` (a renamed `sleep`)
    /// to simulate an agent mid-turn. Registers the agent in the registry
    /// and waits until the window reads as busy so callers can immediately
    /// query liveness. Returns the tmux window target.
    pub fn spawn_fake_agent(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        let target = self.fake_agent_window(project_root, session_name, feature, agent_name);
        crate::tmux::send_keys(
            self.name(),
            &target,
            &format!("exec {} 999", fake_claude().display()),
        )
        .unwrap();
        self.await_liveness(
            &target,
            agent_name,
            crate::commands::running_agents::Liveness::Busy,
        );
        self.register_fake_agent(project_root, feature, agent_name);
        target
    }

    /// [`Self::spawn_fake_agent`] for an agent whose harness has exited: its
    /// window runs only the shell.
    pub fn spawn_dead_fake_agent(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        let target = self.fake_agent_window(project_root, session_name, feature, agent_name);
        self.register_fake_agent(project_root, feature, agent_name);
        target
    }

    fn await_liveness(
        &self,
        target: &str,
        agent_name: &str,
        want: crate::commands::running_agents::Liveness,
    ) {
        use crate::commands::running_agents::liveness;
        let config = crate::state::project::HarnessConfig::default();
        let harness = crate::harness::Harness::ClaudeCode;
        // Under heavy load (parallel tests) this can take longer than
        // usual, so we poll generously.
        for _ in 0..500 {
            if let Ok(processes) = crate::tmux::pane_processes(self.name(), target)
                && liveness(Some(&processes), harness, &config) == want
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for window '{agent_name}' ({target}) to read as {want:?}; \
             pane:\n{:?}",
            crate::tmux::capture_pane(self.name(), target),
        );
    }

    /// [`Self::spawn_fake_agent`] for an agent between turns: its pane runs
    /// a process whose command line carries pm's Stop hook.
    pub fn spawn_idle_fake_agent(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        let target = self.fake_agent_window(project_root, session_name, feature, agent_name);
        // The `; :` keeps `sh` from exec'ing `sleep` in its own place.
        crate::tmux::send_keys(
            self.name(),
            &target,
            &format!(
                "exec sh -c 'sleep 999; :' {}",
                crate::commands::hooks_install::PM_HOOK_MARKER
            ),
        )
        .unwrap();
        self.await_liveness(
            &target,
            agent_name,
            crate::commands::running_agents::Liveness::Idle,
        );
        self.register_fake_agent(project_root, feature, agent_name);
        target
    }

    fn fake_agent_window(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        let target = crate::tmux::new_window(
            self.name(),
            session_name,
            &project_root.join(feature),
            Some(agent_name),
            true,
        )
        .unwrap();
        crate::tmux::mark_agent_pane(self.name(), &target).unwrap();
        target
    }

    fn register_fake_agent(&self, project_root: &std::path::Path, feature: &str, agent_name: &str) {
        use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
        use crate::state::paths;

        let agents_dir = paths::agents_dir(project_root);
        let mut registry = AgentRegistry::load(&agents_dir, feature).unwrap();
        registry.register(
            agent_name,
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: agent_name.to_string(),
                active: true,
                agent_definition: None,
                harness: crate::harness::Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, feature).unwrap();
    }

    /// Add a commit to a feature worktree.
    pub fn add_feature_commit(project_path: &std::path::Path, feature_name: &str) {
        let worktree = project_path.join(feature_name);
        std::fs::write(worktree.join("feature.txt"), "feature work").unwrap();
        crate::git::stage_file(&worktree, "feature.txt").unwrap();
        crate::git::commit(&worktree, "feature work").unwrap();
    }

    /// Leave a rebase paused in `worktree` with a clean tree: every replayed
    /// commit is followed by a failing `--exec`.
    pub fn pause_rebase(worktree: &std::path::Path, onto: &str) {
        let status = std::process::Command::new("git")
            .current_dir(worktree)
            .args(["rebase", "--exec", "false", onto])
            .output()
            .unwrap()
            .status;
        assert!(!status.success());
        assert!(crate::git::rebase_in_progress(worktree).unwrap());
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Kill only sessions whose name starts with our prefix
        let prefix = format!("{}-", self.prefix);
        if let Ok(sessions) = crate::tmux::list_sessions(self.name()) {
            for s in sessions {
                if s.starts_with(&prefix) {
                    let _ = crate::tmux::kill_session(self.name(), &s);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A tmux server on a socket in a private dir, which no other test
    /// binary's reaper scans. Killed on drop, before its dir is removed:
    /// a server whose socket is deleted keeps running, unreachable.
    struct PrivateServer {
        socket: std::path::PathBuf,
    }

    impl PrivateServer {
        fn start(dir: &std::path::Path, name: &str) -> Self {
            let socket = dir.join(name);
            let out = Command::new("tmux")
                .arg("-S")
                .arg(&socket)
                .args([
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    "x",
                    "-c",
                    "/tmp",
                ])
                .arg(HERMETIC_SHELL)
                .output()
                .unwrap();
            assert!(out.status.success(), "start tmux on {socket:?}: {out:?}");
            Self { socket }
        }

        fn running(&self) -> bool {
            Command::new("tmux")
                .arg("-S")
                .arg(&self.socket)
                .arg("has-session")
                .output()
                .is_ok_and(|o| o.status.success())
        }
    }

    impl Drop for PrivateServer {
        fn drop(&mut self) {
            kill_server_at(&self.socket);
        }
    }

    /// A socket dir under `/tmp`: `$TMPDIR` on macOS is long enough to push
    /// a socket path past the 104-byte `sun_path` limit.
    fn private_socket_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("pm-reap-")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn dead_pid() -> u32 {
        let child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        let _ = child.wait_with_output();
        // Very rare flakes possible if the pid is reused immediately.
        assert!(!pid_is_alive(pid), "pid {pid} unexpectedly alive");
        pid
    }

    #[test]
    fn reaper_kills_dead_pid_server() {
        let dir = private_socket_dir();
        let leaked = PrivateServer::start(dir.path(), &format!("pm-test-{}", dead_pid()));

        reap_dead_test_servers(dir.path());

        assert!(!leaked.running(), "reaper left the dead-pid server running");
        assert!(!leaked.socket.exists(), "reaper left the dead-pid socket");
    }

    #[test]
    fn reaper_preserves_live_pid_server() {
        // Wrap the child in an RAII guard so we can't leak the sleep process
        // if an assertion panics mid-test.
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let dir = private_socket_dir();
        let guard = ChildGuard(Command::new("sleep").arg("30").spawn().unwrap());
        let live = PrivateServer::start(dir.path(), &format!("pm-test-{}", guard.0.id()));

        reap_dead_test_servers(dir.path());

        assert!(live.running(), "reaper killed a live-pid server");
    }

    #[test]
    fn reaper_ignores_unrelated_sockets() {
        let dir = private_socket_dir();
        // A file whose pid segment does not parse as an integer. Must be
        // untouched by the reaper (and the reaper must not panic).
        let bogus = dir.path().join("pm-test-notanumber");
        std::fs::File::create(&bogus).unwrap();

        reap_dead_test_servers(dir.path());

        assert!(bogus.exists(), "reaper removed an unparseable filename");
    }

    #[test]
    fn soft_cap_helper_allows_counts_at_or_below_max() {
        // Boundary: exactly MAX_TEST_SESSIONS must NOT trip the cap.
        assert!(enforce_soft_cap(MAX_TEST_SESSIONS, 123).is_ok());
        assert!(enforce_soft_cap(0, 123).is_ok());
        assert!(enforce_soft_cap(1, 123).is_ok());
    }

    #[test]
    fn soft_cap_helper_rejects_counts_above_max_with_recovery_hint() {
        let pid = 4242;
        let err = enforce_soft_cap(MAX_TEST_SESSIONS + 1, pid).expect_err("cap should trip");
        assert!(
            err.contains("pty budget exceeded"),
            "message missing header: {err}"
        );
        assert!(
            err.contains(&format!("tmux -L pm-test-{pid} kill-server")),
            "message missing recovery command: {err}"
        );
        assert!(
            err.contains(&format!("{} sessions", MAX_TEST_SESSIONS + 1)),
            "message missing count: {err}"
        );
    }

    #[test]
    fn soft_cap_panics_when_exceeded() {
        // End-to-end: the production path in TestServer::new() panics when
        // the helper returns Err. We can't realistically push the shared
        // server past 200 sessions inside a unit test, so exercise the
        // panic path indirectly by invoking the same code TestServer::new()
        // does and asserting it panics with the right message.
        let result = std::panic::catch_unwind(|| {
            if let Err(msg) = enforce_soft_cap(MAX_TEST_SESSIONS + 1, std::process::id()) {
                panic!("{msg}");
            }
        });
        let err = result.expect_err("soft cap did not panic");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(
            msg.contains("kill-server"),
            "panic message missing recovery hint: {msg}"
        );
    }
}
