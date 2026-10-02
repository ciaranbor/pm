//! opencode keeps every session of the machine in one database and binds
//! each to a directory. pm reaches them only through opencode's own
//! commands: `session.list` by directory, `session.move` to rebind one,
//! `session export`/`session import` to carry one between stores. Import
//! keeps the session id and leaves a session the store already has alone.
//!
//! A session bound to a directory that no longer exists cannot be resumed,
//! whatever directory opencode is started in, so a moved worktree needs its
//! sessions moved. Paths inside the transcript text are left as recorded.
//!
//! `session.move` answers before the move is carried out, and a
//! `--standalone` call's server exits with the answer, dropping the move. So
//! moves go through a server pm starts for the purpose ([`MoveServer`]) —
//! private to the migration, on loopback, behind a one-off password — and
//! each is confirmed by reading the session back before it is reported.
//!
//! A subagent's session is a session of its own with a `parentID`, and
//! opencode refuses to import one whose parent it lacks (`Not Found`). So
//! an export leaves out a child whose parent is bound to another directory,
//! and an import goes parent before child. A move covers both.

use std::collections::HashSet;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{api, binary, bounded, command, detached, installed_version, refusal, run_api};
use crate::error::{PmError, Result};
use crate::harness::Probe;
use crate::harness::{ImportOutcome, InUse, per_session_outcome};
use crate::state::project::OpenCodeConfig;

const PAGE_SIZE: &str = "limit=100";
const ALREADY_EXISTS: &str = "Session already exists";

const PASSWORD_ENV: &str = "OPENCODE_PASSWORD";
const LISTENING: &str = "server listening on ";
const SERVER_START: Duration = Duration::from_secs(30);
const MOVE_APPLIED: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(200);

struct Session {
    id: String,
    parent: Option<String>,
}

/// `dir` as given and, when it resolves to something else, as opencode
/// records it for a session started there.
fn recorded_forms(dir: &Path) -> Vec<String> {
    let mut forms = vec![dir.to_string_lossy().into_owned()];
    if let Ok(resolved) = dir.canonicalize() {
        let resolved = resolved.to_string_lossy().into_owned();
        if !forms.contains(&resolved) {
            forms.push(resolved);
        }
    }
    forms
}

/// The directory to bind a session to: the form the TUI reports its own
/// location in.
fn binding(dir: &Path) -> String {
    dir.canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Every session bound to exactly `dir`.
fn list(cfg: &OpenCodeConfig, dir: &Path) -> Result<Vec<Session>> {
    let mut sessions: Vec<Session> = Vec::new();
    for directory in recorded_forms(dir) {
        let directory = format!("directory={directory}");
        let mut cursor: Option<String> = None;
        loop {
            let mut args = vec!["session.list", "--param", &directory, "--param", PAGE_SIZE];
            let cursor_param = cursor.as_ref().map(|c| format!("cursor={c}"));
            if let Some(param) = &cursor_param {
                args.extend(["--param", param]);
            }
            let page = api(cfg, &args)?;

            // opencode hands out a next cursor on the last page too, so the
            // end is a page that adds nothing.
            let known = sessions.len();
            for item in page
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(id) = item.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if sessions.iter().all(|s| s.id != id) {
                    sessions.push(Session {
                        id: id.to_string(),
                        parent: item
                            .get("parentID")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                }
            }
            cursor = page
                .pointer("/cursor/next")
                .and_then(Value::as_str)
                .map(str::to_string);
            if sessions.len() == known || cursor.is_none() {
                break;
            }
        }
    }
    Ok(sessions)
}

/// Run `opencode session <verb> <args>` and return what it printed, on
/// either stream.
fn session_command(cfg: &OpenCodeConfig, verb: &str, args: &[&str]) -> Result<(String, String)> {
    let mut command = command(cfg, &["session", verb], args);
    let out = bounded::output(&mut command, &format!("session {verb}"), bounded::TRANSFER)?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Err(PmError::Agent(format!(
            "opencode session {verb} failed: {}",
            refusal(&stdout, &stderr).message
        )));
    }
    Ok((stdout, stderr))
}

/// An opencode server of pm's own that stays up across a migration's
/// moves.
struct MoveServer<'a> {
    cfg: &'a OpenCodeConfig,
    _server: bounded::Guarded,
    url: String,
    password: String,
}

impl<'a> MoveServer<'a> {
    fn start(cfg: &'a OpenCodeConfig) -> Result<Self> {
        let random = || {
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish()
        };
        let password = format!("{:016x}{:016x}", random(), random());

        let mut command = detached(binary(cfg));
        command
            .args(["serve", "--hostname", "127.0.0.1", "--port", "0"])
            .env(PASSWORD_ENV, &password)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut server = bounded::Guarded::spawn(&mut command).map_err(|e| {
            PmError::Agent(format!(
                "could not run `{}` to move sessions: {e}",
                binary(cfg)
            ))
        })?;

        let (sender, announced) = mpsc::channel();
        if let Some(stdout) = server.child.stdout.take() {
            std::thread::spawn(move || {
                // Read to the end: a server writing to a closed pipe dies.
                for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
                    if let Some(url) = line.trim().strip_prefix(LISTENING) {
                        let _ = sender.send(url.to_string());
                    }
                }
            });
        }
        let url = announced.recv_timeout(SERVER_START).map_err(|_| {
            PmError::Agent(
                "opencode serve did not report an address to move sessions through".to_string(),
            )
        })?;
        Ok(Self {
            cfg,
            _server: server,
            url,
            password,
        })
    }

    fn api(&self, args: &[&str]) -> Result<Value> {
        let mut command = detached(binary(self.cfg));
        command
            .args(["api", "--server", &self.url])
            .args(args)
            .env(PASSWORD_ENV, &self.password);
        run_api(command, args)
    }

    /// Rebind `session` to `directory` and wait until opencode has.
    fn move_session(&self, session: &str, directory: &str) -> Result<()> {
        let param = format!("sessionID={session}");
        let body = json!({"directory": directory}).to_string();
        self.api(&["session.move", "--param", &param, "--data", &body])?;

        let asked = Instant::now();
        loop {
            let found = self.api(&["session.get", "--param", &param])?;
            if found
                .pointer("/data/location/directory")
                .and_then(Value::as_str)
                == Some(directory)
            {
                return Ok(());
            }
            if asked.elapsed() > MOVE_APPLIED {
                return Err(PmError::Agent(format!(
                    "opencode accepted the move of session {session} to {directory} but had \
                     not carried it out after {}s",
                    MOVE_APPLIED.as_secs()
                )));
            }
            std::thread::sleep(POLL);
        }
    }
}

pub(in crate::harness) fn unreachable(cfg: &OpenCodeConfig) -> Option<String> {
    installed_version(cfg, Probe::Fresh).err()
}

pub(in crate::harness) fn migrate(
    cfg: &OpenCodeConfig,
    from: &Path,
    to: &Path,
    in_use: &[InUse],
) -> Result<Vec<String>> {
    // `to` reached through a symlink to `from`: the binding is already it.
    if recorded_forms(from).contains(&binding(to)) {
        return Ok(vec![format!(
            "opencode sessions of {} are bound to the directory {} resolves to",
            from.display(),
            to.display()
        )]);
    }
    let sessions = list(cfg, from)?;
    if sessions.is_empty() {
        return Ok(vec![format!(
            "No opencode sessions found for {}",
            from.display()
        )]);
    }

    let mut report = Vec::new();
    let mut free = Vec::new();
    for session in &sessions {
        match in_use.iter().find(|u| u.session_id == session.id) {
            Some(holder) => report.push(format!(
                "Left session {} at {}: agent {} is running on it; stop the agent and migrate \
                 again",
                session.id,
                from.display(),
                holder.agent
            )),
            None => free.push(session),
        }
    }
    if free.is_empty() {
        return Ok(report);
    }

    // A failure must not swallow what was and was not moved before it.
    let directory = binding(to);
    let mut moved = 0;
    let mut failures = Vec::new();
    match MoveServer::start(cfg) {
        Ok(server) => {
            for session in &free {
                match server.move_session(&session.id, &directory) {
                    Ok(()) => moved += 1,
                    Err(e) => failures.push(format!("Session {} was not moved: {e}", session.id)),
                }
            }
        }
        Err(e) => failures.push(e.to_string()),
    }
    if moved > 0 || failures.is_empty() {
        report.push(format!(
            "Moved {moved} opencode session(s) from {} to {}",
            from.display(),
            to.display()
        ));
    }
    if failures.is_empty() {
        return Ok(report);
    }
    report.extend(failures);
    Err(PmError::Agent(report.join("\n")))
}

pub(in crate::harness) fn export(
    cfg: &OpenCodeConfig,
    dir: &Path,
    staging: &Path,
) -> Result<Option<String>> {
    let mut sessions = list(cfg, dir)?;
    // A child whose parent is not exported with it could not be imported;
    // repeat until no session's parent is missing, for deeper nesting.
    let listed = sessions.len();
    loop {
        let before = sessions.len();
        let ids: HashSet<String> = sessions.iter().map(|s| s.id.clone()).collect();
        sessions.retain(|s| s.parent.as_ref().is_none_or(|p| ids.contains(p)));
        if sessions.len() == before {
            break;
        }
    }
    if sessions.is_empty() {
        return Ok(None);
    }

    std::fs::create_dir_all(staging)?;
    for session in &sessions {
        let (exported, _) = session_command(cfg, "export", &[&session.id])?;
        std::fs::write(staging.join(format!("{}.json", session.id)), exported)?;
    }
    let mut detail = format!("{} session(s)", sessions.len());
    let orphans = listed - sessions.len();
    if orphans > 0 {
        detail.push_str(&format!(
            "; {orphans} subagent session(s) not exported: parent bound elsewhere"
        ));
    }
    Ok(Some(detail))
}

/// `files` (exported sessions) ordered parent before child. A file whose
/// session can't be read goes where it is, for opencode to refuse.
fn parents_first(files: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut pending = Vec::new();
    for file in files {
        let info = serde_json::from_slice::<Value>(&std::fs::read(&file)?)
            .ok()
            .and_then(|v| v.get("info").cloned());
        let field = |key: &str| {
            info.as_ref()
                .and_then(|i| i.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        pending.push((file, field("id"), field("parentID")));
    }
    let staged: HashSet<String> = pending.iter().filter_map(|(_, id, _)| id.clone()).collect();
    let mut done: HashSet<String> = HashSet::new();
    let mut ordered = Vec::new();
    while !pending.is_empty() {
        let (ready, waiting): (Vec<_>, Vec<_>) = pending.into_iter().partition(|(_, _, parent)| {
            parent
                .as_ref()
                .is_none_or(|p| !staged.contains(p) || done.contains(p))
        });
        if ready.is_empty() {
            ordered.extend(waiting.into_iter().map(|(file, _, _)| file));
            break;
        }
        for (file, id, _) in ready {
            done.extend(id);
            ordered.push(file);
        }
        pending = waiting;
    }
    Ok(ordered)
}

pub(in crate::harness) fn import(
    cfg: &OpenCodeConfig,
    staging: &Path,
    to: &Path,
) -> Result<ImportOutcome> {
    let mut files: Vec<_> = std::fs::read_dir(staging)?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    let files = parents_first(files)?;

    let directory = binding(to);
    let mut imported = 0;
    for file in &files {
        // opencode exits 0 for a session it already has, and says so.
        let (stdout, stderr) = session_command(
            cfg,
            "import",
            &["--directory", &directory, &file.to_string_lossy()],
        )?;
        if !stdout.contains(ALREADY_EXISTS) && !stderr.contains(ALREADY_EXISTS) {
            imported += 1;
        }
    }
    Ok(per_session_outcome(imported, files.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{fake_opencode, fake_opencode_calls, fake_opencode_sequence};

    fn cfg(binary: String) -> OpenCodeConfig {
        OpenCodeConfig {
            binary: Some(binary),
            ..Default::default()
        }
    }

    fn page(ids: &[&str], next: Option<&str>) -> String {
        let data: Vec<Value> = ids.iter().map(|id| json!({"id": id})).collect();
        json!({"data": data, "cursor": {"previous": null, "next": next}}).to_string()
    }

    /// The value of `--param <name>=…` in a recorded call.
    fn param<'a>(call: &'a [String], name: &str) -> Option<&'a str> {
        call.windows(2)
            .filter(|pair| pair[0] == "--param")
            .find_map(|pair| pair[1].strip_prefix(name)?.strip_prefix('='))
    }

    const SERVING: &str = "server listening on http://127.0.0.1:9";

    /// What `session.get` answers for a session bound to `dir`.
    fn bound_to(dir: &Path) -> String {
        json!({"data": {"id": "ses", "location": {"directory": dir.canonicalize().unwrap()}}})
            .to_string()
    }

    /// The sessions the recorded calls asked opencode to move.
    fn moved(calls: &[Vec<String>]) -> Vec<&str> {
        calls
            .iter()
            .filter(|call| call.iter().any(|arg| arg == "session.move"))
            .map(|call| param(call, "sessionID").unwrap())
            .collect()
    }

    #[test]
    fn migrate_moves_every_session_of_the_old_directory_to_the_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("new");
        std::fs::create_dir_all(&to).unwrap();
        let bound = bound_to(&to);
        let cfg = cfg(fake_opencode_sequence(
            dir.path(),
            &[
                &page(&["ses_a", "ses_b"], None),
                SERVING,
                "",
                &bound,
                "",
                &bound,
            ],
            0,
        ));

        // The old directory is gone, as after a moved worktree.
        let messages = migrate(&cfg, Path::new("/gone/old"), &to, &[]).unwrap();

        let calls = fake_opencode_calls(dir.path());
        assert_eq!(&calls[0][..3], ["api", "--standalone", "session.list"]);
        assert_eq!(param(&calls[0], "directory"), Some("/gone/old"));
        // A `--standalone` server would exit before carrying a move out.
        assert_eq!(
            calls[1],
            ["serve", "--hostname", "127.0.0.1", "--port", "0"]
        );
        for (call, id) in [(&calls[2], "ses_a"), (&calls[4], "ses_b")] {
            assert_eq!(
                call[..4],
                ["api", "--server", "http://127.0.0.1:9", "session.move"]
            );
            assert_eq!(param(call, "sessionID"), Some(id));
            let at = call.iter().position(|a| a == "--data").unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&call[at + 1]).unwrap(),
                json!({"directory": to.canonicalize().unwrap()})
            );
        }
        assert_eq!(
            messages,
            [format!(
                "Moved 2 opencode session(s) from /gone/old to {}",
                to.display()
            )]
        );
    }

    #[test]
    fn a_move_is_reported_only_once_opencode_has_carried_it_out() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let cfg = cfg(fake_opencode_sequence(
            dir.path(),
            &[
                &page(&["ses_a"], None),
                SERVING,
                "",
                &bound_to(elsewhere.path()),
                &bound_to(elsewhere.path()),
                &bound_to(dir.path()),
            ],
            0,
        ));

        migrate(&cfg, Path::new("/gone/old"), dir.path(), &[]).unwrap();

        let calls = fake_opencode_calls(dir.path());
        let reads = calls
            .iter()
            .filter(|call| call.iter().any(|arg| arg == "session.get"))
            .count();
        assert_eq!(reads, 3, "{calls:?}");
    }

    #[test]
    fn migrate_leaves_a_session_an_agent_is_running_on() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(fake_opencode_sequence(
            dir.path(),
            &[
                &page(&["ses_a", "ses_b"], None),
                SERVING,
                "",
                &bound_to(dir.path()),
            ],
            0,
        ));
        let in_use = [InUse {
            session_id: "ses_a".to_string(),
            agent: "login/reviewer".to_string(),
        }];

        let messages = migrate(&cfg, Path::new("/gone/old"), dir.path(), &in_use).unwrap();

        assert_eq!(moved(&fake_opencode_calls(dir.path())), ["ses_b"]);
        assert!(
            messages[0].starts_with("Left session ses_a at /gone/old: agent login/reviewer"),
            "{messages:?}"
        );
        assert!(messages[1].starts_with("Moved 1 opencode session(s)"));
    }

    #[test]
    fn migrate_to_a_symlink_of_the_old_directory_asks_opencode_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let link = dir.path().join("main");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        let cfg = cfg(fake_opencode(dir.path(), &page(&["ses_a"], None), 0));

        let messages = migrate(&cfg, &repo.canonicalize().unwrap(), &link, &[]).unwrap();

        assert!(
            messages[0].contains("are bound to the directory"),
            "{messages:?}"
        );
        assert!(fake_opencode_calls(dir.path()).is_empty());
    }

    #[test]
    fn migrate_with_no_sessions_says_so_and_moves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(fake_opencode(dir.path(), &page(&[], None), 0));
        let messages = migrate(&cfg, Path::new("/gone/old"), dir.path(), &[]).unwrap();
        assert_eq!(messages, ["No opencode sessions found for /gone/old"]);
        assert_eq!(fake_opencode_calls(dir.path()).len(), 1);
    }

    #[test]
    fn listing_follows_the_cursor_until_a_page_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // The last page still names a next cursor, as opencode's does.
        let cfg = cfg(fake_opencode_sequence(
            dir.path(),
            &[
                &page(&["ses_a", "ses_b"], Some("cur1")),
                &page(&["ses_c"], Some("cur2")),
                &page(&[], None),
            ],
            0,
        ));

        let ids: Vec<String> = list(&cfg, Path::new("/gone/old"))
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();

        assert_eq!(ids, ["ses_a", "ses_b", "ses_c"]);
        let calls = fake_opencode_calls(dir.path());
        let cursors: Vec<_> = calls.iter().map(|call| param(call, "cursor")).collect();
        assert_eq!(cursors, [None, Some("cur1"), Some("cur2")]);
    }

    #[test]
    fn export_carries_subagent_sessions_whose_parent_it_carries() {
        let dir = tempfile::tempdir().unwrap();
        let listing = json!({
            "data": [
                {"id": "ses_a", "parentID": "ses_b"},
                {"id": "ses_b", "parentID": null},
                {"id": "ses_orphan", "parentID": "ses_elsewhere"},
                {"id": "ses_orphans_child", "parentID": "ses_orphan"},
            ],
            "cursor": {"previous": null, "next": null},
        })
        .to_string();
        let cfg = cfg(fake_opencode_sequence(
            dir.path(),
            &[&listing, r#"{"info":{"id":"ses_x"},"messages":[]}"#],
            0,
        ));
        let staging = dir.path().join("staging");

        let detail = export(&cfg, Path::new("/gone/proj"), &staging).unwrap();

        assert_eq!(
            detail.as_deref(),
            Some("2 session(s); 2 subagent session(s) not exported: parent bound elsewhere")
        );
        let calls = fake_opencode_calls(dir.path());
        assert_eq!(
            calls[1..],
            [
                ["session", "export", "--standalone", "ses_a"],
                ["session", "export", "--standalone", "ses_b"],
            ]
        );
        for id in ["ses_a", "ses_b"] {
            let written = std::fs::read_to_string(staging.join(format!("{id}.json"))).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&written).unwrap(),
                json!({"info": {"id": "ses_x"}, "messages": []})
            );
        }
        for id in ["ses_orphan", "ses_orphans_child"] {
            assert!(!staging.join(format!("{id}.json")).exists(), "{id}");
        }
    }

    #[test]
    fn export_of_a_directory_without_sessions_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(fake_opencode(dir.path(), &page(&[], None), 0));
        let staging = dir.path().join("staging");
        assert_eq!(
            export(&cfg, Path::new("/gone/proj"), &staging).unwrap(),
            None
        );
        assert!(!staging.exists());
    }

    #[test]
    fn import_binds_each_session_to_the_local_directory() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        let local = dir.path().join("main");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(staging.join("ses_a.json"), "{}").unwrap();
        std::fs::write(staging.join("ses_b.json"), "{}").unwrap();
        let cfg = cfg(fake_opencode_sequence(
            dir.path(),
            &["Imported session ses_a", ALREADY_EXISTS],
            0,
        ));

        let outcome = import(&cfg, &staging, &local).unwrap();

        assert_eq!(
            outcome,
            ImportOutcome::Imported {
                detail: "1 session(s), 1 already present".to_string(),
                notes: Vec::new(),
            }
        );
        let local = local.canonicalize().unwrap();
        let calls = fake_opencode_calls(dir.path());
        assert_eq!(calls.len(), 2);
        for (call, file) in calls.iter().zip(["ses_a.json", "ses_b.json"]) {
            assert_eq!(
                call[..],
                [
                    "session",
                    "import",
                    "--standalone",
                    "--directory",
                    local.to_str().unwrap(),
                    staging.join(file).to_str().unwrap(),
                ]
            );
        }
    }

    #[test]
    fn import_puts_each_parent_session_before_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        let local = dir.path().join("main");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        for (id, parent) in [
            ("ses_a", Some("ses_b")),
            ("ses_b", Some("ses_c")),
            ("ses_c", None),
        ] {
            let info = json!({"info": {"id": id, "parentID": parent}, "messages": []});
            std::fs::write(staging.join(format!("{id}.json")), info.to_string()).unwrap();
        }
        let cfg = cfg(fake_opencode(dir.path(), "", 0));

        import(&cfg, &staging, &local).unwrap();

        let order: Vec<String> = fake_opencode_calls(dir.path())
            .iter()
            .map(|call| call.last().unwrap().rsplit('/').next().unwrap().to_string())
            .collect();
        assert_eq!(order, ["ses_c.json", "ses_b.json", "ses_a.json"]);
    }

    #[test]
    fn a_failed_command_surfaces_opencodes_own_message() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("ses_a.json"), "{}").unwrap();
        let cfg = cfg(fake_opencode(dir.path(), "Session not found: ses_a", 1));
        let err = import(&cfg, &staging, dir.path()).unwrap_err().to_string();
        assert!(
            err.ends_with("opencode session import failed: Session not found: ses_a"),
            "{err}"
        );
    }

    #[test]
    fn a_server_that_never_announces_an_address_fails_the_migration_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(fake_opencode_sequence(
            dir.path(),
            &[&page(&["ses_a", "ses_b"], None), "", ""],
            0,
        ));
        let in_use = [InUse {
            session_id: "ses_a".to_string(),
            agent: "login/reviewer".to_string(),
        }];

        let asked = Instant::now();
        let err = migrate(&cfg, Path::new("/gone/old"), dir.path(), &in_use)
            .unwrap_err()
            .to_string();

        assert!(asked.elapsed() < SERVER_START / 2, "{:?}", asked.elapsed());
        assert!(err.contains("did not report an address"), "{err}");
        assert!(err.contains("Left session ses_a"), "{err}");
        assert!(moved(&fake_opencode_calls(dir.path())).is_empty());
    }

    #[test]
    fn a_failed_move_still_reports_the_sessions_around_it() {
        let dir = tempfile::tempdir().unwrap();
        // `ses_a` is read back bound to the target; the answer to `ses_b`'s
        // move is not one opencode gives.
        let cfg = cfg(fake_opencode_sequence(
            dir.path(),
            &[
                &page(&["ses_a", "ses_b"], None),
                SERVING,
                "",
                &bound_to(dir.path()),
                "not json",
            ],
            0,
        ));

        let err = migrate(&cfg, Path::new("/gone/old"), dir.path(), &[])
            .unwrap_err()
            .to_string();

        assert!(err.contains("Moved 1 opencode session(s)"), "{err}");
        assert!(err.contains("Session ses_b was not moved"), "{err}");
    }

    #[test]
    fn migrate_with_every_session_in_use_moves_nothing_and_claims_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(fake_opencode(dir.path(), &page(&["ses_a"], None), 0));
        let in_use = [InUse {
            session_id: "ses_a".to_string(),
            agent: "login/reviewer".to_string(),
        }];

        let messages = migrate(&cfg, Path::new("/gone/old"), dir.path(), &in_use).unwrap();

        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].starts_with("Left session ses_a"));
        assert_eq!(fake_opencode_calls(dir.path()).len(), 1);
    }
}
