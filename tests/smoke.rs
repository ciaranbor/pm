//! Smoke tests: the real `pm` binary inside a `scripts/sandbox` (isolated
//! `$HOME`, private tmux server, harness shims on `PATH`). Each scenario
//! covers behaviour that depends on where a command is run from (cwd, the
//! real config dir, inherited env, inside its own tmux window) — nothing a
//! lib test can reach. A scenario earns its place only by such a failure
//! mode, never by mirroring a lib test. Ignored by default:
//! `cargo test --test smoke -- --ignored`.

mod sandbox;

use std::path::{Path, PathBuf};
use std::time::Instant;

use assert_cmd::Command;

use predicates::prelude::*;
use sandbox::*;

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

/// Catches: `pm init` run outside tmux not reaching the new main session
/// with its `main` agent, or starting it outside the main worktree.
#[test]
#[ignore]
fn init_starts_main_in_the_main_session() {
    let s = Smoke::new();
    s.pm(s.home())
        .args(["init", &s.proj().to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Spawned agent 'main'"));

    let records = s.argv_records("main", 1);
    assert_eq!(records[0].agent_name, "main");
    assert_eq!(Path::new(&records[0].cwd), s.proj().join("main"));
    assert_eq!(s.window_names("proj/main"), ["main"]);
}

/// Catches: `pm notes` not finding its editor in the inherited env — an
/// empty `$VISUAL` taken over `$EDITOR`, or an `$EDITOR` with arguments not
/// run through the shell.
#[test]
#[ignore]
fn notes_opens_the_editor_from_the_env_with_its_arguments() {
    let s = Smoke::new();
    s.pm(s.home())
        .args(["init", "--no-main", &s.proj().to_string_lossy()])
        .assert()
        .success();
    let source = s.home().join("from editor.md");
    std::fs::write(&source, "written by the editor\n").unwrap();

    s.pm(&s.proj().join("main"))
        .arg("notes")
        .env("VISUAL", "")
        .env("EDITOR", format!("cp '{}'", source.display()))
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(s.proj().join(".pm/notes.md")).unwrap(),
        "written by the editor\n"
    );

    s.pm(s.home())
        .args(["notes", "proj"])
        .env("VISUAL", "false")
        .assert()
        .failure()
        .stderr(predicate::str::contains("false exited"));
}

/// Catches: `pm init --git <url>` without a PATH not cloning into
/// `./<repo name>` under the caller's cwd.
#[test]
#[ignore]
fn init_from_git_without_a_path_roots_the_project_in_the_cwd() {
    let s = Smoke::new();
    let staging = s.home().join("staging");
    s.git(
        s.home(),
        &["init", "-q", "-b", "main", &staging.to_string_lossy()],
    );
    s.git(&staging, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let remote = s.home().join("remotes/app.git");
    s.git(
        s.home(),
        &[
            "clone",
            "-q",
            "--bare",
            &staging.to_string_lossy(),
            &remote.to_string_lossy(),
        ],
    );
    let cwd = s.home().join("work");
    std::fs::create_dir(&cwd).unwrap();

    s.pm(&cwd)
        .args(["init", "--no-main", "--git", &remote.to_string_lossy()])
        .assert()
        .success();

    assert!(cwd.join("app/main/.git").exists());
    assert!(s.projects_dir().join("app.toml").exists());
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
/// panes killing that pane, and so itself, before the other agents
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
    let old = s.tmux_ok(&["display", "-p", "-t", &old, "#{pane_id}"]);
    // Stop the shim so the window's shell (which still exports
    // PM_AGENT_NAME) takes commands again.
    s.tmux_ok(&["send-keys", "-t", &old, "C-c", ""]);
    s.wait_for_shell(&old);

    let outcome = s.run_in(&old, "pm agent restart reviewer helper");
    assert!(!outcome.alive, "old pane survived the restart: {outcome:?}");
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

/// Catches: `agent restart --all --global` run from an agent's own pane
/// killing it before the agents of later scopes restart: the caller's
/// scope, `main`, comes before `login` but must restart last.
#[test]
#[ignore]
fn restart_all_global_from_inside_an_agents_own_window_restarts_it_last() {
    let s = Smoke::new();
    s.init_with_spawned_reviewer();
    let main = s.proj().join("main");
    s.pm(&main)
        .args(["agent", "spawn", "scout", "--agent", "implementer"])
        .assert()
        .success();
    s.argv_records("reviewer", 1);
    s.argv_records("scout", 1);

    let old = s.find_window("proj/main", "scout").expect("scout window");
    let old = s.tmux_ok(&["display", "-p", "-t", &old, "#{pane_id}"]);
    s.tmux_ok(&["send-keys", "-t", &old, "C-c", ""]);
    s.wait_for_shell(&old);

    let outcome = s.run_in(&old, "pm agent restart --all --global");
    assert!(!outcome.alive, "old pane survived the restart: {outcome:?}");
    let lines: Vec<&str> = outcome.log.lines().collect();
    assert_eq!(lines.len(), 3, "{}", outcome.log);
    assert!(
        lines[0].starts_with("proj/login: Restarted agent 'reviewer'"),
        "{}",
        outcome.log
    );
    assert!(
        lines[1].starts_with("proj/main: Restarted agent 'scout'"),
        "{}",
        outcome.log
    );
    assert_eq!(lines[2], "Restarted 2, skipped 0, failed 0");
    for agent in ["reviewer", "scout"] {
        assert_eq!(s.argv_records(agent, 2).len(), 2, "{agent}");
    }
    assert!(s.find_window("proj/main", "scout").is_some());
}

/// Catches: `pm upgrade` run from a stale agent's own pane — as the
/// post-merge hook runs it — killing that agent mid-command instead of
/// reporting it, while restarting the other stale agents.
#[test]
#[ignore]
fn upgrade_from_inside_a_stale_agents_window_reports_it_and_restarts_the_rest() {
    let s = Smoke::new();
    s.init_with_spawned_reviewer();
    let main = s.proj().join("main");
    s.pm(&main)
        .args(["agent", "spawn", "scout", "--agent", "implementer"])
        .assert()
        .success();
    s.argv_records("reviewer", 1);
    s.argv_records("scout", 1);
    std::fs::write(s.proj().join(".pm/notices.md"), "Be terse.").unwrap();

    let old = s.find_window("proj/main", "scout").expect("scout window");
    let old = s.tmux_ok(&["display", "-p", "-t", &old, "#{pane_id}"]);
    s.tmux_ok(&["send-keys", "-t", &old, "C-c", ""]);
    s.wait_for_shell(&old);

    let outcome = s.run_in(&old, "pm upgrade");
    assert!(outcome.alive, "the caller's pane was killed: {outcome:?}");
    assert!(
        outcome.log.contains(
            "proj/main: Skipped agent 'scout': it is stale, and runs this command; restart it \
             with `pm agent restart scout --scope main`"
        ),
        "{}",
        outcome.log
    );
    assert!(
        outcome
            .log
            .contains("proj/login: Restarted agent 'reviewer'"),
        "{}",
        outcome.log
    );
    assert_eq!(s.argv_records("reviewer", 2).len(), 2);
    assert_eq!(s.argv_records("scout", 1).len(), 1);
}

/// Catches: tmux output parsed under launchd's environment — no UTF-8
/// locale and no `$TMUX` — where tmux prints tabs as `_` and every live
/// session reads as closed.
#[test]
#[ignore]
fn status_sees_live_sessions_without_a_utf8_locale() {
    let s = Smoke::new();
    s.init_with_feature();
    let out = s
        .pm(&s.proj().join("main"))
        .arg("status")
        .env_remove("LANG")
        .env_remove("LC_ALL")
        .env_remove("LC_CTYPE")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8_lossy(&out);
    let login = out
        .lines()
        .find(|l| l.trim_start().starts_with("login"))
        .unwrap_or_else(|| panic!("no login row: {out}"));
    assert!(!login.contains("no session"), "{out}");
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
    assert!(
        outcome.log.contains("Deleted project 'proj'"),
        "reported before the kill: {}",
        outcome.log
    );

    let sessions = s.sessions();
    assert!(
        !sessions.iter().any(|n| n.starts_with("proj/")),
        "sessions: {sessions:?}"
    );
    assert!(sessions.iter().any(|n| n == "keepalive"));
    assert!(!registry_entry.exists(), "registry entry left behind");
    assert!(!proj.exists(), "project root left behind");
}

/// Catches: `pm delete` run from a feature's session, with and without
/// `--force`, which killed that session, and so itself, before `.pm/` and
/// the registry entry went.
#[test]
#[ignore]
fn delete_from_inside_a_feature_session() {
    for args in ["--yes", "--force --yes"] {
        let s = Smoke::new();
        let proj = s.proj();
        s.init_with_feature();
        let registry_entry = s.projects_dir().join("proj.toml");

        let outcome = s.run_in("proj/login:0", &format!("pm delete {args}"));
        assert!(!outcome.alive, "{args}: {outcome:?}");
        assert!(
            outcome.log.contains("Deleted project 'proj'"),
            "{args}: {}",
            outcome.log
        );

        let sessions = s.sessions();
        assert!(
            !sessions.iter().any(|n| n.starts_with("proj/")),
            "{args}: sessions: {sessions:?}"
        );
        assert!(
            !registry_entry.exists(),
            "{args}: registry entry left behind"
        );
        assert!(!proj.join(".pm").exists(), "{args}: .pm/ left behind");
    }
}

/// Catches: `pm feat delete` and `pm feat merge` run from the feature's own
/// session, which killed it, and so themselves, before they reported.
#[test]
#[ignore]
fn feature_delete_and_merge_from_inside_its_own_session() {
    for (command, said) in [
        ("pm feat delete", "Deleted feature 'login'"),
        ("pm feat merge", "Merged and deleted feature 'login'"),
    ] {
        let s = Smoke::new();
        let login = s.init_with_feature();

        let outcome = s.run_in("proj/login:0", command);
        assert!(!outcome.alive, "{command}: {outcome:?}");
        assert!(outcome.log.contains(said), "{command}: {}", outcome.log);

        let sessions = s.sessions();
        assert!(
            !sessions.iter().any(|n| n == "proj/login"),
            "{command}: sessions: {sessions:?}"
        );
        assert!(sessions.iter().any(|n| n == "proj/main"), "{sessions:?}");
        assert!(!login.exists(), "{command}: worktree left behind");
    }
}

/// Catches: `pm close` run from a feature session, whose kill ends the
/// caller, leaving the sessions it had not reached open, or before it
/// reports.
#[test]
#[ignore]
fn close_from_inside_a_feature_session() {
    let s = Smoke::new();
    s.init_with_feature();
    s.pm(&s.proj().join("main"))
        .args(["feat", "new", "api"])
        .assert()
        .success();

    let outcome = s.run_in("proj/api:0", "pm close");
    assert!(!outcome.alive, "{outcome:?}");
    assert!(
        outcome
            .log
            .contains("Closed project proj (killed 3 sessions)"),
        "{}",
        outcome.log
    );

    let sessions = s.sessions();
    assert!(
        !sessions.iter().any(|n| n.starts_with("proj/")),
        "sessions: {sessions:?}"
    );
}

/// Catches: `pm close --all` run from a project closed before the others,
/// whose kill ends the caller before it reaches them.
#[test]
#[ignore]
fn close_all_from_inside_a_project_that_is_not_last() {
    let s = Smoke::new();
    s.init_with_feature();
    s.pm(s.home())
        .args(["init", "--no-main", &s.home().join("zzz").to_string_lossy()])
        .assert()
        .success();

    let outcome = s.run_in("proj/main:0", "pm close --all");
    assert!(!outcome.alive, "{outcome:?}");

    let sessions = s.sessions();
    assert!(
        !sessions
            .iter()
            .any(|n| n.starts_with("proj/") || n.starts_with("zzz/")),
        "sessions: {sessions:?}"
    );
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
        .args(["init", "--no-main", &s.proj().to_string_lossy()])
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
             --version*) echo 2.0.23 ;;\n\
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
        .args(["init", "--no-main", &proj.to_string_lossy()])
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
        .args(["init", "--no-main", &proj.to_string_lossy()])
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

/// Catches: install reaching launchd through `launchctl` on the PATH (the
/// sandbox's recording shim, which keeps it off the user's own launchd
/// domain) and writing the plist under the sandbox's HOME.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn serve_install_loads_its_agent_through_launchctl() {
    let s = Smoke::new();
    let plist = s.home().join("Library/LaunchAgents/dev.pm.serve.plist");

    s.pm(s.home())
        .args(["serve", "install", "--no-tailscale"])
        .assert()
        .success()
        .stdout(predicate::str::contains(plist.display().to_string()))
        .stdout(predicate::str::contains("tailscale is not connected"));

    let calls = std::fs::read_to_string(s.home().join("log/launchctl.argv")).unwrap();
    // SAFETY: getuid cannot fail.
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    assert!(
        calls.contains(&format!("bootstrap {domain} {}", plist.display())),
        "{calls}"
    );
    assert!(plist.exists());
}

/// Catches: the background push a pm command starts waking `pm serve`
/// through the wake FIFO in the real config dir, which the command finds
/// only from its own environment. What the server does when woken is a
/// lib test's.
#[test]
#[ignore]
fn a_change_pm_makes_wakes_the_server() {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let s = Smoke::new();
    s.init_with_feature();
    let fifo = s.projects_dir().with_file_name("serve").join("wake");
    std::fs::create_dir_all(fifo.parent().unwrap()).unwrap();
    let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // Safety: `path` is a valid NUL-terminated string for the call.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let mut server = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();

    s.pm(&s.proj().join("login"))
        .args(["feat", "status", "blocked", "-m", "which DB?"])
        .assert()
        .success();

    let start = Instant::now();
    let mut byte = [0];
    while !matches!(server.read(&mut byte), Ok(1)) {
        assert!(start.elapsed() < WAIT, "the push never woke the server");
        std::thread::sleep(POLL);
    }
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
    // The shim runs no SessionStart hook, so nothing clears the marker that
    // would read the exited harness as still starting.
    std::fs::remove_file(s.proj().join(".pm/runtime/login/reviewer/waiting.json"))
        .expect("the spawn's startup marker");
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

/// Catches: the move to a new machine end to end — `pm migrate check`
/// gating on unpushed work, `pm state init --global --remote` on a machine
/// with no config dir, after a mistyped remote, and again where `pm init`
/// already registered a project and the registry was already pulled, and
/// `pm restore --import`
/// putting a conversation in place before it respawns the agent that
/// resumes it.
#[test]
#[ignore]
fn a_project_moves_to_a_fresh_machine_and_its_agent_resumes() {
    let s = Smoke::new();
    let login = s.init_with_feature();
    let main = s.proj().join("main");
    s.pm(&login)
        .args(["agent", "spawn", "reviewer"])
        .assert()
        .success();
    s.argv_records("reviewer", 1);

    // The reviewer has a conversation, recorded under its resolved cwd.
    let agents = s.proj().join(".pm/agents/login.toml");
    let text: Vec<String> = std::fs::read_to_string(&agents)
        .unwrap()
        .lines()
        .map(|l| {
            if l.starts_with("session_id = ") {
                "session_id = \"sess-1\"".to_string()
            } else {
                l.to_string()
            }
        })
        .collect();
    std::fs::write(&agents, text.join("\n") + "\n").unwrap();
    let resolved = login.canonicalize().unwrap();
    let key: String = resolved
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let store = s.home().join(".claude/projects").join(key);
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("sess-1.jsonl"),
        format!("{{\"cwd\":\"{}\"}}\n", resolved.display()),
    )
    .unwrap();

    let remotes = s.home().join("remotes");
    let remote = |name: &str| {
        let path = remotes.join(name);
        s.git(s.home(), &["init", "-q", "--bare", &path.to_string_lossy()]);
        path.to_string_lossy().to_string()
    };
    let (repo, state, registry) = (
        remote("proj.git"),
        remote("state.git"),
        remote("registry.git"),
    );
    s.git(&main, &["remote", "add", "origin", &repo]);
    s.git(&main, &["push", "-q", "-u", "origin", "main"]);

    s.pm(&main)
        .args(["state", "remote", &state])
        .assert()
        .success();
    s.pm(&main).args(["state", "push"]).assert().success();
    s.pm(s.home())
        .args(["state", "init", "--global", "--remote", &registry])
        .assert()
        .success();
    s.pm(s.home())
        .args(["state", "backfill"])
        .assert()
        .success();
    s.pm(s.home())
        .args(["state", "push", "--global"])
        .assert()
        .success();
    s.pm(s.home()).args(["close", "--all"]).assert().success();

    // A branch with no commits of its own is created from its base on the
    // new host; one with work blocks until it is pushed.
    s.git(
        &login,
        &["commit", "-q", "--allow-empty", "-m", "login work"],
    );
    s.pm(s.home())
        .args(["migrate", "check", "--project", "proj"])
        .assert()
        .failure()
        .stdout(predicate::str::is_match(r"login/ +not on origin").unwrap());
    s.git(&main, &["push", "-q", "-u", "origin", "login"]);
    s.pm(s.home())
        .args(["migrate", "check", "--project", "proj"])
        .assert()
        .success();

    let tarball = s.home().join("pm-claude-code.tar.gz");
    s.pm(s.home())
        .args([
            "harness",
            "export",
            "--all",
            "-o",
            &tarball.to_string_lossy(),
        ])
        .assert()
        .success();

    // The new machine: nothing of pm's, nor the conversation, at the same
    // paths.
    std::fs::remove_dir_all(s.proj()).unwrap();
    std::fs::remove_dir_all(s.projects_dir().parent().unwrap()).unwrap();
    std::fs::remove_dir_all(s.home().join(".claude/projects")).unwrap();

    // Set up before the move: a project of its own, and the registry
    // pulled once already, so pulling it again must not fail.
    s.pm(s.home())
        .args([
            "init",
            "--no-main",
            &s.home().join("other").to_string_lossy(),
        ])
        .assert()
        .success();
    // A mistyped URL first: it fails and leaves nothing a retry trips on.
    let typo = remotes.join("registry-typo.git");
    s.pm(s.home())
        .args([
            "state",
            "init",
            "--global",
            "--remote",
            &typo.to_string_lossy(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing was changed"));
    for _ in 0..2 {
        s.pm(s.home())
            .args(["state", "init", "--global", "--remote", &registry])
            .assert()
            .success();
    }
    s.pm(s.home())
        .args([
            "restore",
            "--project",
            "proj",
            "--import",
            &tarball.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "recreated worktree for feature 'login'",
        ))
        .stdout(predicate::str::contains("Imported 'proj/login'"));

    let records = s.argv_records("reviewer", 2);
    let resumed = records.last().unwrap();
    assert!(
        resumed.argv.windows(2).any(|w| w == ["--resume", "sess-1"]),
        "{resumed:?}"
    );
    assert_eq!(resumed.resumed.as_deref(), Some("found"), "{resumed:?}");
    s.pm(s.home())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("other"));
}

/// A static file server of `dir` on loopback, for as long as the test runs;
/// its URL.
fn serve_dir(dir: &Path) -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr().to_ip().unwrap());
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            let file = dir.join(request.url().trim_start_matches('/'));
            let _ = match std::fs::read(&file) {
                Ok(bytes) => request.respond(tiny_http::Response::from_data(bytes)),
                Err(_) => request.respond(tiny_http::Response::empty(404)),
            };
        }
    });
    url
}

/// Catches: the install script against a real HOME and PATH — a first
/// install into `~/.local/bin` off PATH, a re-run finding that pm on PATH
/// and upgrading in place, a checksum mismatch installing nothing, and a
/// missing binary naming what to do.
#[test]
#[ignore]
fn the_install_script_installs_reinstalls_and_refuses_a_bad_checksum() {
    use sha2::Digest;
    let s = Smoke::new();
    let release = tempfile::tempdir().unwrap();
    let asset = format!("pm-{}", pm::version::TARGET);
    let binary = std::fs::read(env!("CARGO_BIN_EXE_pm")).unwrap();
    std::fs::write(release.path().join(&asset), &binary).unwrap();
    let sums = |bytes: &[u8]| {
        let digest: String = sha2::Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(
            release.path().join("SHA256SUMS"),
            format!("{digest}  {asset}\n"),
        )
        .unwrap();
    };
    sums(&binary);
    let url = serve_dir(release.path());
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh");
    let installed = s.home().join(".local/bin/pm");
    let install = |path: &str| {
        let mut cmd = Command::from_std(s.run_cmd(s.home(), "env"));
        cmd.args([
            &format!("PATH={path}"),
            &format!("PM_DOWNLOAD_URL={url}"),
            "sh",
            script,
        ]);
        cmd
    };

    install("/usr/bin:/bin")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Installed pm {}",
            env!("CARGO_PKG_VERSION")
        )))
        .stdout(predicate::str::contains("run-shell 'pm tmux init'"))
        .stderr(predicate::str::contains("is not on your PATH"));
    assert_eq!(std::fs::read(&installed).unwrap(), binary);

    let on_path = format!("{}:/usr/bin:/bin", installed.parent().unwrap().display());
    install(&on_path)
        .assert()
        .success()
        .stdout(predicate::str::contains(installed.display().to_string()))
        .stdout(predicate::str::contains("No registered projects"))
        .stdout(predicate::str::contains("run-shell").not())
        .stderr(predicate::str::contains("not on your PATH").not());
    assert_eq!(std::fs::read(&installed).unwrap(), binary);

    sums(b"something else");
    install(&on_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not match SHA256SUMS"));
    assert_eq!(std::fs::read(&installed).unwrap(), binary);
    let left: Vec<_> = std::fs::read_dir(installed.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["pm"]);

    std::fs::remove_file(release.path().join(&asset)).unwrap();
    let missing = install(&on_path).assert().failure();
    let stderr = String::from_utf8_lossy(&missing.get_output().stderr).into_owned();
    assert!(stderr.contains("build from source"), "{stderr}");
    assert_eq!(
        stderr.contains("try again later"),
        pm::version::TARGET.contains("apple-darwin"),
        "{stderr}"
    );
}
