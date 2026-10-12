//! Real-harness tests: the built `pm` against the real `claude`, `codex`
//! and `opencode` in a `scripts/sandbox up --real` (isolated `$HOME`,
//! private tmux server). They cover what only the harness itself can show —
//! where it keeps sessions, what it records, what it asks — with as few
//! model calls as possible: the opencode scenarios make none, the others
//! one or two on the cheapest model. Ignored by default:
//! `cargo test --test real_harness -- --ignored`.
//!
//! A scenario whose harness is not on `PATH`, or not logged in the way
//! `scripts/sandbox --help` describes, fails saying so: a pass always means
//! the harness ran. To run only some harnesses, filter by name (every
//! scenario starts with its harness: `-- --ignored opencode`).

mod sandbox;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sandbox::*;

const TURN: Duration = Duration::from_secs(180);

/// Why `harness` cannot run here, if it cannot.
fn missing(harness: &str) -> Option<String> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let on_path = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(harness).is_file()));
    if !on_path {
        return Some(format!("`{harness}` is not on PATH"));
    }
    let set = |name| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    match harness {
        "claude" => {
            let token = std::env::var_os("PM_SANDBOX_CLAUDE_TOKEN_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config/pm-secrets/claude-token"));
            (!set("CLAUDE_CODE_OAUTH_TOKEN") && !set("ANTHROPIC_API_KEY") && !token.is_file()).then(
                || {
                    "no Claude Code credential (CLAUDE_CODE_OAUTH_TOKEN, ANTHROPIC_API_KEY or \
                     a token file; see scripts/sandbox --help)"
                        .to_string()
                },
            )
        }
        "codex" => (!home.join(".codex/auth.json").is_file())
            .then(|| "codex is not logged in (~/.codex/auth.json)".to_string()),
        _ => None,
    }
}

fn require(harness: &str) {
    if let Some(why) = missing(harness) {
        panic!("cannot run {harness} here: {why}");
    }
}

fn wait_for(what: &str, limit: Duration, s: &Smoke, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < limit,
            "timed out waiting for {what}\n{}",
            s.capture_all()
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

impl Smoke {
    /// A committed git repository at `dir` under the sandbox HOME.
    fn repo(&self, dir: &str) -> PathBuf {
        let path = self.home().join(dir);
        std::fs::create_dir_all(&path).unwrap();
        self.git(&path, &["init", "-q"]);
        self.git(&path, &["commit", "-q", "--allow-empty", "-m", "init"]);
        path
    }

    fn opencode_api(&self, cwd: &Path, args: &[&str]) -> serde_json::Value {
        let out = self
            .run_cmd(cwd, "opencode")
            .args(["api", "--standalone"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "opencode api {args:?}: {out:?}");
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// A new opencode session bound to `dir`. No model is involved.
    fn opencode_session(&self, dir: &Path) -> String {
        let created = self.opencode_api(dir, &["session.create", "--data", "{}"]);
        created["data"]["id"].as_str().unwrap().to_string()
    }

    /// The ids of the opencode sessions bound to `dir`.
    fn opencode_sessions_at(&self, dir: &Path) -> Vec<String> {
        let dir = dir.canonicalize().unwrap();
        let listed = self.opencode_api(
            self.home(),
            &[
                "session.list",
                "--param",
                &format!("directory={}", dir.display()),
            ],
        );
        listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn global_config(&self) -> PathBuf {
        self.projects_dir().parent().unwrap().join("config.toml")
    }

    fn registry(&self, project: &Path, scope: &str) -> String {
        std::fs::read_to_string(project.join(format!(".pm/agents/{scope}.toml")))
            .unwrap_or_default()
    }

    fn session_id(&self, project: &Path, scope: &str) -> Option<String> {
        self.registry(project, scope)
            .lines()
            .find_map(|l| l.strip_prefix("session_id = \""))
            .map(|rest| rest.trim_end_matches('"').to_string())
            .filter(|id| !id.is_empty())
    }

    /// `pm init --no-main` a project at `name` whose every agent runs on
    /// `harness`, with `model` if given.
    fn project_on(&self, name: &str, harness: &str, model: Option<&str>) -> PathBuf {
        let root = self.home().join(name);
        self.pm(self.home())
            .args(["init", "--no-main", &root.to_string_lossy()])
            .assert()
            .success();
        let config = root.join(".pm/config.toml");
        let mut text = std::fs::read_to_string(&config).unwrap().replace(
            "[agents.harness]\n",
            &format!("[agents.harness]\n\"*\" = \"{harness}\"\n"),
        );
        if let Some(model) = model {
            text = text.replace(
                "[agents.models]\n",
                &format!("[agents.models]\n\"*\" = \"{model}\"\n"),
            );
        }
        std::fs::write(&config, text).unwrap();
        root
    }

    /// Pretend `scope`'s agents were spawned long ago, past doctor's grace.
    fn backdate_spawns(&self, project: &Path, scope: &str) {
        let file = project.join(format!(".pm/agents/{scope}.toml"));
        let text = std::fs::read_to_string(&file).unwrap();
        let text: String = text
            .lines()
            .map(|l| {
                if l.starts_with("spawned_at = ") {
                    "spawned_at = \"2000-01-01T00:00:00Z\"".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&file, text + "\n").unwrap();
    }

    fn doctor(&self, cwd: &Path) -> String {
        let out = self.pm(cwd).arg("doctor").output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
    }
}

/// Catches: `pm register --move` stranding sessions of a harness the new
/// project's config does not (yet) name — a session made before pm stays
/// bound to the vacated path, where opencode cannot resume it.
#[test]
#[ignore]
fn opencode_register_move_carries_its_sessions() {
    require("opencode");
    let s = Smoke::real();
    let repo = s.repo("ocrepo");
    let session = s.opencode_session(&repo);

    s.pm(s.home())
        .args(["register", "--move", "--no-main", &repo.to_string_lossy()])
        .assert()
        .success()
        .stderr(predicates::str::contains("Moved 1 opencode session(s)"));

    assert_eq!(s.opencode_sessions_at(&repo.join("main")), [session]);
}

/// Catches: `pm feat adopt --from` leaving the old worktree's opencode
/// sessions bound to it.
#[test]
#[ignore]
fn opencode_adopt_carries_the_old_worktrees_sessions() {
    require("opencode");
    let s = Smoke::real();
    let proj = s.project_on("proj", "opencode", Some("local/none"));
    let main = proj.join("main");
    let old = s.home().join("oldwt");
    s.git(&main, &["branch", "carry"]);
    s.git(
        &main,
        &["worktree", "add", "-q", &old.to_string_lossy(), "carry"],
    );
    let session = s.opencode_session(&old);
    s.git(&main, &["worktree", "remove", &old.to_string_lossy()]);

    s.pm(&main)
        .args(["feat", "adopt", "carry", "--from", &old.to_string_lossy()])
        .assert()
        .success()
        .stderr(predicates::str::contains("Moved 1 opencode session(s)"));

    assert_eq!(s.opencode_sessions_at(&proj.join("carry")), [session]);
}

/// Catches: a move opencode refuses being lost — the command it was part
/// of must still succeed, say which session stayed and how to retry, and
/// the retry must then carry it. The refusal is real: the move server runs
/// on an empty store (the binary is wrapped so only `serve` sees one), so
/// opencode answers `Session not found`.
#[test]
#[ignore]
fn opencode_a_failed_move_is_reported_and_can_be_retried() {
    require("opencode");
    let s = Smoke::real();
    let repo = s.repo("ocrepo");
    let session = s.opencode_session(&repo);
    let real = s
        .run_cmd(s.home(), "sh")
        .args(["-c", "command -v opencode"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    let wrapper = s.home().join("bin/opencode-empty-serve");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n[ \"$1\" = serve ] && export XDG_DATA_HOME=\"$HOME/empty-store\"\n\
             exec '{real}' \"$@\"\n"
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::create_dir_all(s.global_config().parent().unwrap()).unwrap();
    std::fs::write(
        s.global_config(),
        format!("[harness.opencode]\nbinary = \"{}\"\n", wrapper.display()),
    )
    .unwrap();

    let out = s
        .pm(s.home())
        .args(["register", "--move", "--no-main", &repo.to_string_lossy()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let main = repo.join("main");
    for expected in [
        format!(
            "Warning: opencode sessions of {} were not all carried to {}",
            repo.display(),
            main.display()
        ),
        format!("Session {session} was not moved: opencode session.move failed: Session not found"),
        format!(
            "To retry, run `pm harness migrate --harness opencode --from {}` in {}",
            repo.display(),
            main.display()
        ),
    ] {
        assert!(stderr.contains(&expected), "{expected}\nin:\n{stderr}");
    }
    assert!(!stderr.contains("Agent error: Session"), "{stderr}");
    assert!(s.opencode_sessions_at(&main).is_empty());

    std::fs::remove_file(s.global_config()).unwrap();
    s.pm(&main)
        .args(["harness", "migrate", "--harness", "opencode", "--from"])
        .arg(&repo)
        .assert()
        .success()
        .stdout(predicates::str::contains("Moved 1 opencode session(s)"));
    assert_eq!(s.opencode_sessions_at(&main), [session]);
}

/// Catches: a Claude Code agent launched with no prompt that a message does
/// not wake, from its spawn or from a resume; a resume with nothing unread
/// that is not told to carry on; and a restart that starts it afresh
/// instead of resuming — a new session id, or a second transcript — or that
/// resumes a session that never took a turn, which has no transcript. Three
/// turns on the cheapest model: one per message, and the resume notice's.
#[test]
#[ignore]
fn claude_restart_resumes_the_recorded_session() {
    require("claude");
    let s = Smoke::real();
    let proj = s.project_on("proj", "claude-code", Some("haiku"));
    let main = proj.join("main");
    sandbox_ok(&s.name, &["trust", &main.to_string_lossy()]);

    s.pm(&main)
        .args(["agent", "spawn", "plain"])
        .assert()
        .success();
    wait_for("the session id", TURN, &s, || {
        s.session_id(&proj, "main").is_some()
    });
    let unturned = s.session_id(&proj, "main").unwrap();
    let idle = || {
        wait_for("the agent idle", TURN, &s, || {
            pm::state::runtime::read_waiting(&proj, "main", "plain")
                .is_some_and(|w| w.kind == pm::state::runtime::WaitingKind::Idle)
        });
    };
    idle();
    s.pm(&main)
        .args(["agent", "restart", "plain"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "claude-code has no conversation for session {unturned}; previous session not \
             resumed"
        )));
    wait_for("a new session id", TURN, &s, || {
        s.session_id(&proj, "main").is_some_and(|id| id != unturned)
    });
    let id = s.session_id(&proj, "main").unwrap();
    let transcripts = || -> Vec<PathBuf> {
        std::fs::read_dir(s.home().join(".claude/projects"))
            .into_iter()
            .flatten()
            .flatten()
            .flat_map(|dir| {
                std::fs::read_dir(dir.path())
                    .into_iter()
                    .flatten()
                    .flatten()
            })
            .map(|f| f.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect()
    };
    let transcript = |text: &str| {
        transcripts().iter().any(|t| {
            std::fs::read_to_string(t)
                .unwrap_or_default()
                .contains(text)
        })
    };
    let message = |text: &str| {
        idle();
        s.pm(&main)
            .args(["msg", "send", "plain", text])
            .assert()
            .success();
    };
    message("first-message: reply with OK and nothing else.");
    wait_for("the first message's turn", TURN, &s, || {
        transcript("first-message") && transcript("\"type\":\"assistant\"")
    });

    // Restarted mid-turn, it would be woken by the restart's own notice.
    idle();
    s.pm(&main)
        .args(["agent", "restart", "--force", "plain"])
        .assert()
        .success()
        .stdout(predicates::str::contains("resumed session"));
    wait_for("the resume notice in the transcript", TURN, &s, || {
        transcript("pm resumed this session")
    });
    message("second-message: reply with OK and nothing else.");
    wait_for("the second message's turn", TURN, &s, || {
        transcript("second-message")
    });

    assert_eq!(s.session_id(&proj, "main").as_deref(), Some(id.as_str()));
    let all = transcripts();
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].file_stem().unwrap().to_string_lossy(), id);
    s.pm(&main)
        .args(["agent", "stop", "plain"])
        .assert()
        .success();
}

/// Catches: an agent the user interrupted, or one whose waiter is gone,
/// left asleep with a message waiting, or woken by keys typed into its
/// window. A message sent after an Esc wakes it through the waiter its
/// prompt started; with no waiter, `pm msg send` posts the continuation to
/// Claude Code's messaging socket, which reaches a pane in copy mode. A
/// few turns on the cheapest model.
#[test]
#[ignore]
fn claude_an_interrupted_or_unarmed_agent_is_woken_without_typing() {
    use pm::state::runtime::{self, SessionPath, WaitingKind};
    require("claude");
    let s = Smoke::real();
    let proj = s.project_on("proj", "claude-code", Some("haiku"));
    let main = proj.join("main");
    sandbox_ok(&s.name, &["trust", &main.to_string_lossy()]);
    s.pm(&main)
        .args(["agent", "spawn", "plain"])
        .assert()
        .success();
    let idle = || {
        wait_for("the agent idle", TURN, &s, || {
            runtime::read_waiting(&proj, "main", "plain")
                .is_some_and(|w| w.kind == WaitingKind::Idle)
        });
    };
    let transcript = || {
        runtime::read_session_path(&proj, "main", "plain", SessionPath::Transcript)
            .and_then(|t| std::fs::read_to_string(t).ok())
            .unwrap_or_default()
    };
    let session = s
        .sessions()
        .into_iter()
        .find(|n| n.contains("main"))
        .unwrap();
    let window = s.find_window(&session, "plain").unwrap();
    let send = |text: &str| {
        let out = s
            .pm(&main)
            .args(["msg", "send", "plain", text])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    idle();
    assert!(runtime::read_inbound(&proj, "main", "plain").is_some());

    send("first: write a 400-word story about a lighthouse.");
    wait_for("the woken turn", TURN, &s, || {
        transcript().contains("Stop hook feedback")
    });
    s.tmux_ok(&["send-keys", "-t", &window, "Escape"]);
    wait_for("the interrupt", TURN, &s, || {
        transcript().contains("[Request interrupted by user")
    });
    std::thread::sleep(Duration::from_secs(2));
    let before = transcript().len();
    send("second: reply with the word kiwi and nothing else.");
    wait_for("a turn after the interrupt", TURN, &s, || {
        transcript()[before..].contains("\"type\":\"assistant\"")
    });

    idle();
    let waiter = std::fs::read_to_string(proj.join(".pm/runtime/main/plain/waiter")).unwrap();
    let waiter = waiter.split_whitespace().next().unwrap().to_string();
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &waiter])
            .status()
            .unwrap()
            .success()
    );
    s.tmux_ok(&["copy-mode", "-t", &window]);
    let before = transcript().len();
    let status = send("third: reply with the word plum and nothing else.");
    assert!(
        status.contains("Re-armed 'plain' (its waiter is gone)"),
        "{status}"
    );
    wait_for("the posted continuation's turn", TURN, &s, || {
        let new = &transcript()[before..];
        new.contains("\"turnOrigin\":\"peer\"") && new.contains("\"type\":\"assistant\"")
    });
    assert_eq!(
        s.tmux_ok(&["display", "-p", "-t", &window, "#{pane_in_mode}"]),
        "1",
        "nothing was typed into the pane"
    );
    s.pm(&main)
        .args(["agent", "stop", "plain"])
        .assert()
        .success();
}

/// Catches: the spawn and doctor missing what codex's interactive hook-trust
/// gate does to an agent — before trust its hooks never run, so it never
/// comes up and its session never starts; after trust, a fresh spawn
/// and a resume come up, and a pm upgrade that changes a hook's command
/// makes codex distrust it again, which only codex can tell (its
/// `trusted_hash` is its own fingerprint). A few turns on codex's default
/// model.
#[test]
#[ignore]
fn codex_hook_trust_through_the_interactive_gate_and_a_changed_hook() {
    require("codex");
    let s = Smoke::real();
    let proj = s.project_on("proj", "codex", None);
    let main = proj.join("main");
    let window = "proj/main:plain";

    s.pm(&main)
        .args(["agent", "spawn", "plain"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "its codex harness started but has not come up after 20s",
        ))
        .stderr(predicates::str::contains("Trust all and continue"));
    s.backdate_spawns(&proj, "main");
    let doctor = s.doctor(&main);
    for expected in [
        "agent 'plain' is running but its codex session never reported starting",
        "codex has not trusted pm's Stop hook",
    ] {
        assert!(doctor.contains(expected), "{expected}\nin:\n{doctor}");
    }

    s.tmux_ok(&["send-keys", "-t", window, "2", "Enter"]);
    wait_for("the session id", TURN, &s, || {
        s.session_id(&proj, "main").is_some()
    });
    let doctor = s.doctor(&main);
    assert!(!doctor.contains("trust"), "{doctor}");
    // Trusted, a fresh codex comes up: its session starts by the first
    // turn, which the spawn prompt begins at once.
    s.pm(&main)
        .args(["agent", "spawn", "fresh", "--agent", "plain"])
        .assert()
        .success();
    s.pm(&main)
        .args(["agent", "stop", "fresh"])
        .assert()
        .success();

    let hooks = s.home().join(".codex/hooks.json");
    let original = std::fs::read_to_string(&hooks).unwrap();
    let changed = original.replacen(
        "exec pm harness hooks stop codex",
        "exec pm harness hooks stop  codex",
        1,
    );
    assert_ne!(changed, original);
    std::fs::write(&hooks, changed).unwrap();
    let doctor = s.doctor(&main);
    assert!(
        doctor.contains("codex trusts only an earlier version of pm's Stop hook"),
        "{doctor}"
    );
    assert!(!doctor.contains("SessionStart hook"), "{doctor}");
    std::fs::write(&hooks, original).unwrap();
    // A resumed codex session starts again, so the restart sees it come up.
    s.pm(&main)
        .args(["agent", "restart", "--force", "plain"])
        .assert()
        .success()
        .stdout(predicates::str::contains("resumed session"));
    s.pm(&main)
        .args(["agent", "stop", "plain"])
        .assert()
        .success();
}

/// Catches: a codex async question (`request_user_input_async`) not read as
/// asking, or its answer from the phone not reaching codex as the reply its
/// own question panel gives — so the model never learns it.
#[test]
#[ignore]
fn codex_async_question_reads_asking_and_is_answered_through_serve() {
    require("codex");
    let s = Smoke::real();
    let proj = s.project_on("proj", "codex", None);
    let config = proj.join(".pm/config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let text = match text.contains("[harness.codex]\n") {
        true => text.replace(
            "[harness.codex]\n",
            "[harness.codex]\nbypass_hook_trust = true\n",
        ),
        false => text + "\n[harness.codex]\nbypass_hook_trust = true\n",
    };
    std::fs::write(&config, text).unwrap();
    let main = proj.join("main");
    s.pm(&main)
        .args(["agent", "spawn", "plain"])
        .assert()
        .success();

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let url = format!("http://127.0.0.1:{port}");
    let paired = s
        .pm(s.home())
        .args(["serve", "pair", "--url", &url, "--name", "test"])
        .output()
        .unwrap();
    let token = String::from_utf8_lossy(&paired.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("token:").map(|t| t.trim().to_string()))
        .expect("pair prints the token");
    let mut server = s
        .run_cmd(s.home(), "pm")
        .args(["serve", "--port", &port.to_string()])
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let agent = format!("{url}/v1/agents/proj/main/plain");
    let request = |method: &str, path: &str, body: Option<serde_json::Value>| {
        let mut curl = std::process::Command::new("curl");
        curl.args(["-s", "-X", method, "-H"])
            .arg(format!("Authorization: Bearer {token}"))
            .arg(format!("{agent}/{path}"));
        if let Some(body) = body {
            curl.args(["-H", "Content-Type: application/json", "-d"])
                .arg(body.to_string());
        }
        let out = curl.output().unwrap();
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap_or_default()
    };
    wait_for("pm serve", Duration::from_secs(20), &s, || {
        request("GET", "dialogs", None).get("dialogs").is_some()
    });

    let asked = request(
        "POST",
        "input",
        Some(serde_json::json!({"text":
            "Use the request_user_input_async tool to ask me one multiple-choice question: \
             \"Which colour?\" with options Red and Blue. Then wait for my answer with \
             clock.sleep (up to 5 minutes) and reply with only the colour I chose."})),
    );
    assert!(asked.get("delivery").is_some(), "{asked}");
    let mut dialog = serde_json::Value::Null;
    wait_for("the question", TURN, &s, || {
        dialog = request("GET", "dialog", None);
        dialog.get("id").is_some()
    });
    assert_eq!(dialog["kind"], "question", "{dialog}");
    let question = dialog["questions"][0]["question"]
        .as_str()
        .unwrap()
        .to_string();
    let plain = || {
        let mut curl = std::process::Command::new("curl");
        curl.args(["-s", "-H"])
            .arg(format!("Authorization: Bearer {token}"))
            .arg(format!("{url}/v1/snapshot"));
        let snapshot: serde_json::Value =
            serde_json::from_slice(&curl.output().unwrap().stdout).unwrap_or_default();
        snapshot["projects"][0]["main"]["agents"]
            .as_array()
            .and_then(|agents| agents.iter().find(|a| a["name"] == "plain").cloned())
            .unwrap_or_default()
    };
    let asking = plain();
    assert_eq!(asking["state"], "asking", "{asking}");
    assert_eq!(asking["waiting"]["detail"], question.as_str(), "{asking}");
    assert_eq!(asking["waiting"]["dialog"], dialog["id"], "{asking}");

    let answered = request(
        "POST",
        "dialog",
        Some(serde_json::json!({"id": dialog["id"], "choice": "answer",
                                "answers": {question.as_str(): "Blue"}})),
    );
    assert_eq!(answered, serde_json::json!({"answered": true}));
    wait_for("the model's reply, idle", TURN, &s, || {
        let page = request("GET", "transcript?limit=10", None);
        let replied = page["items"].as_array().is_some_and(|items| {
            items.iter().any(|i| {
                i["kind"] == "assistant" && i["text"].as_str().unwrap_or("").contains("Blue")
            })
        });
        replied && plain()["state"] == "idle"
    });
    assert_eq!(
        request("GET", "dialogs", None)["dialogs"],
        serde_json::json!([])
    );

    let _ = server.kill();
    let _ = server.wait();
    s.pm(&main)
        .args(["agent", "stop", "plain"])
        .assert()
        .success();
}

/// Catches: a codex agent not woken through its own app-server — after Esc
/// ends a turn (no Stop hook runs), whether the message came before the Esc
/// or after it — and pm's sandbox lost on a resume or a fork, where the TUI
/// takes no permission flags: a read-only agent must still write pm's
/// state and nothing else. Then that stopping the agents leaves no server
/// to run a wake as a turn nobody sees. A few turns on codex's default
/// model.
#[test]
#[ignore]
fn codex_is_woken_without_typing_and_keeps_its_sandbox() {
    use pm::state::runtime::{self, WaitingKind};
    require("codex");
    let s = Smoke::real();
    let proj = s.project_on("proj", "codex", None);
    let config = proj.join(".pm/config.toml");
    let text = std::fs::read_to_string(&config).unwrap().replace(
        "[agents.permissions]\n",
        "[agents.permissions]\nplain = \"read-only\"\n",
    );
    std::fs::write(&config, text).unwrap();
    let main = proj.join("main");
    let messages = proj.join(".pm/messages");
    let unread = |agent: &str| pm::messages::unread_count(&messages, "main", agent);
    let marker = |agent: &str| runtime::read_waiting(&proj, "main", agent).map(|w| w.kind);
    let send = |agent: &str, text: &str| {
        let out = s
            .pm(&main)
            .args(["msg", "send", agent, text])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let idle_and_read = |agent: &str| {
        wait_for("the agent to read its messages and idle", TURN, &s, || {
            unread(agent) == 0 && marker(agent) == Some(WaitingKind::Idle)
        });
    };

    // Trust pm's hooks once, through codex's own gate, so no resume stops
    // at its review.
    let _ = s.pm(&main).args(["agent", "spawn", "plain"]).output();
    s.tmux_ok(&["send-keys", "-t", "proj/main:plain", "2", "Enter"]);
    idle_and_read("plain");

    send(
        "plain",
        "Run the shell command `sleep 60` in the foreground, then reply done.",
    );
    wait_for("the turn", TURN, &s, || {
        unread("plain") == 0 && marker("plain").is_none()
    });
    std::thread::sleep(Duration::from_secs(8));
    send("plain", "Reply with the word plum and nothing else.");
    s.tmux_ok(&["send-keys", "-t", "proj/main:plain", "Escape"]);
    idle_and_read("plain");

    send(
        "plain",
        "Run the shell command `sleep 60` in the foreground, then reply done.",
    );
    wait_for("the turn", TURN, &s, || {
        unread("plain") == 0 && marker("plain").is_none()
    });
    std::thread::sleep(Duration::from_secs(8));
    s.tmux_ok(&["send-keys", "-t", "proj/main:plain", "Escape"]);
    wait_for("the interrupt", TURN, &s, || {
        marker("plain") == Some(WaitingKind::Interrupted)
    });
    let status = send("plain", "Reply with the word fig and nothing else.");
    assert!(
        status.contains("Re-armed 'plain' (interrupted)"),
        "{status}"
    );
    idle_and_read("plain");

    let writes = |agent: &str| {
        let allowed = proj.join(format!(".pm/{agent}-allowed"));
        let denied = main.join(format!("{agent}-denied"));
        send(
            agent,
            &format!(
                "Run exactly these two shell commands, each on its own, and reply with their \
                 exit statuses: `touch {}` and `touch {}`.",
                allowed.display(),
                denied.display()
            ),
        );
        idle_and_read(agent);
        assert!(allowed.exists(), "{agent} could not write pm's state");
        assert!(!denied.exists(), "{agent} wrote its read-only worktree");
    };
    s.pm(&main)
        .args(["agent", "restart", "--force", "plain"])
        .assert()
        .success()
        .stdout(predicates::str::contains("resumed session"));
    idle_and_read("plain");
    writes("plain");
    s.pm(&main)
        .args(["agent", "fork", "plain", "twin"])
        .assert()
        .success();
    idle_and_read("twin");
    writes("twin");

    s.pm(&main)
        .args(["agent", "stop", "plain", "twin"])
        .assert()
        .success();
    let servers = || {
        let out = std::process::Command::new("pgrep")
            .args([
                "-f",
                &format!("app-server --listen unix://{}", proj.display()),
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    wait_for("the servers to end", Duration::from_secs(10), &s, || {
        servers().is_empty()
    });
}

/// Feature `guard` of a project whose agents run on `harness` with `model`
/// and the `[agents.permissions]` row `permissions`, with a `plain` agent
/// spawned in it. Returns the project root.
fn feature_agent(
    s: &Smoke,
    harness: &str,
    model: Option<&str>,
    permissions: Option<&str>,
) -> PathBuf {
    let proj = s.project_on("proj", harness, model);
    if let Some(row) = permissions {
        s.set_agents_config("permissions", &format!("\"*\" = \"{row}\""));
    }
    s.pm(&proj.join("main"))
        .args(["feat", "new", "guard"])
        .assert()
        .success();
    let feature = proj.join("guard");
    sandbox_ok(&s.name, &["trust", &feature.to_string_lossy()]);
    // A first-run dialog keeps the spawn from reporting the agent up; the
    // caller answers it.
    let spawned = s
        .pm(&feature)
        .args(["agent", "spawn", "plain"])
        .output()
        .unwrap();
    eprintln!("spawn: {spawned:?}");
    proj
}

/// Asks the feature agent to create a file in main's checkout and one in
/// its own worktree with its file-editing tool, `typed` at its window (a
/// model may decline such a request from a message) or else sent as one:
/// only its own may appear.
fn edits_only_its_own_worktree(s: &Smoke, proj: &Path, typed: bool) {
    use pm::state::runtime::{self, WaitingKind};
    // Nested, so a rule that matches only one level below main fails.
    let denied = proj.join("main/src/guard-denied.txt");
    std::fs::create_dir_all(denied.parent().unwrap()).unwrap();
    let allowed = proj.join("guard/guard-allowed.txt");
    let idle = || {
        runtime::read_waiting(proj, "guard", "plain").is_some_and(|w| w.kind == WaitingKind::Idle)
    };
    wait_for("the agent to idle", TURN, s, idle);
    let request = format!(
        "With your file-editing tool, never the shell, create the file {} holding the word x, \
         then create the file {} holding the word x. If one is refused, do not retry it \
         another way.",
        denied.display(),
        allowed.display()
    );
    if typed {
        s.tmux_ok(&["send-keys", "-t", "proj/guard:plain", "-l", &request]);
        std::thread::sleep(Duration::from_secs(1));
        s.tmux_ok(&["send-keys", "-t", "proj/guard:plain", "Enter"]);
    } else {
        s.pm(&proj.join("guard"))
            .args(["msg", "send", "plain", &request])
            .assert()
            .success();
    }
    wait_for("the edit to its own worktree", TURN, s, || allowed.exists());
    wait_for("the turn to end", TURN, s, idle);
    assert!(
        !denied.exists(),
        "the agent edited main's checkout\n{}",
        s.capture_all()
    );
}

/// Catches: a feature's Claude Code agent editing main's checkout with its
/// file tools, in the mode that approves everything else. One turn on the
/// cheapest model.
#[test]
#[ignore]
fn claude_a_feature_agent_cannot_edit_main() {
    require("claude");
    let s = Smoke::real();
    let settings = s.home().join(".claude/settings.json");
    let mut root: serde_json::Value = std::fs::read_to_string(&settings)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    root["skipDangerousModePermissionPrompt"] = true.into();
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, root.to_string()).unwrap();
    let proj = feature_agent(&s, "claude-code", Some("haiku"), Some("bypassPermissions"));
    edits_only_its_own_worktree(&s, &proj, true);
}

/// Catches: a feature's codex agent editing main's checkout with
/// apply_patch, under the default full-access sandbox where only pm's
/// guard hook stands in the way. pm's hooks are trusted through codex's own
/// gate. A turn or two on codex's default model.
#[test]
#[ignore]
fn codex_a_feature_agent_cannot_edit_main() {
    require("codex");
    let s = Smoke::real();
    let proj = feature_agent(&s, "codex", None, None);
    s.tmux_ok(&["send-keys", "-t", "proj/guard:plain", "2", "Enter"]);
    edits_only_its_own_worktree(&s, &proj, false);
}

/// Catches: a feature's opencode agent editing main's checkout under
/// `--auto`. Needs an OpenAI-compatible server at 127.0.0.1:8000 serving a
/// model with tool calls; its first model is used.
#[test]
#[ignore]
fn opencode_a_feature_agent_cannot_edit_main() {
    require("opencode");
    let listed = std::process::Command::new("curl")
        .args(["-s", "-m", "5", "http://127.0.0.1:8000/v1/models"])
        .output()
        .unwrap();
    let models: serde_json::Value = serde_json::from_slice(&listed.stdout)
        .unwrap_or_else(|_| panic!("cannot run opencode here: no model server at 127.0.0.1:8000"));
    let model = models["data"][0]["id"].as_str().unwrap().to_string();
    let s = Smoke::real();
    std::fs::create_dir_all(s.global_config().parent().unwrap()).unwrap();
    std::fs::write(
        s.global_config(),
        std::fs::read_to_string(s.global_config()).unwrap_or_default()
            + &format!(
                "\n[harness.opencode.providers.local]\n\
                 package = \"@opencode/ai/providers/openai-compatible\"\n\
                 settings = {{ baseURL = \"http://127.0.0.1:8000/v1\" }}\n\
                 [harness.opencode.providers.local.models.\"{model}\"]\n"
            ),
    )
    .unwrap();
    let proj = feature_agent(&s, "opencode", Some(&format!("local/{model}")), None);
    edits_only_its_own_worktree(&s, &proj, false);
}
