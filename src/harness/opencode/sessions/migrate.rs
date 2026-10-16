//! A session bound to a directory that no longer exists cannot be resumed,
//! whatever directory opencode is started in, so a moved worktree needs its
//! sessions moved. Paths inside the transcript text are left as recorded.
//!
//! `session.move` answers before the move is carried out, and a
//! `--standalone` call's server exits with the answer, dropping the move. So
//! moves go through a server pm starts for the purpose ([`MoveServer`]) —
//! private to the migration, on loopback, behind a one-off password — and
//! each is confirmed by reading the session back before it is reported.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::super::api::{detached, run_api};
use super::super::{PASSWORD_ENV, binary, random_password};
use super::{binding, list};
use crate::bounded;
use crate::error::{PmError, Result};
use crate::harness::InUse;
use crate::harness::session_store::recorded_forms;
use crate::state::project::OpenCodeConfig;

const LISTENING: &str = "server listening on ";
const SERVER_START: Duration = Duration::from_secs(30);
const MOVE_APPLIED: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(200);

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
        let password = random_password()?;

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
                    Err(e) => failures.push(format!(
                        "Session {} was not moved: {}",
                        session.id,
                        e.reason()
                    )),
                }
            }
        }
        Err(e) => failures.push(e.reason()),
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

#[cfg(test)]
mod tests {
    use super::super::tests::{cfg, page, param};
    use super::*;
    use crate::testing::{fake_opencode, fake_opencode_calls, fake_opencode_sequence};

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
