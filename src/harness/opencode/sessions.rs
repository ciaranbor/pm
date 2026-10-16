//! opencode keeps every session of the machine in one database and binds
//! each to a directory. pm reaches them only through opencode's own
//! commands: `session.list` by directory, `session.move` to rebind one,
//! `session export`/`session import` to carry one between stores. Import
//! keeps the session id and leaves a session the store already has alone.
//! A moved worktree's sessions are rebound by [`migrate`](mod@migrate).
//!
//! A subagent's session is a session of its own with a `parentID`, and
//! opencode refuses to import one whose parent it lacks (`Not Found`). So
//! an export leaves out a child whose parent is bound to another directory,
//! and an import goes parent before child. A move covers both.
//!
//! A spawn's session is also created or forked here, and the agent's model
//! row pinned on it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::api::{TRANSFER, api, command, refusal, try_api};
use super::config::ModelRef;
use super::installed_version;
use crate::bounded;
use crate::error::{PmError, Result};
use crate::harness::session_store::recorded_forms;
use crate::harness::{ImportOutcome, per_session_outcome};
use crate::harness::{LaunchContext, Probe, SpawnSpec};
use crate::state::project::OpenCodeConfig;

mod migrate;
pub(in crate::harness) use migrate::migrate;

const PAGE_SIZE: &str = "limit=100";
const ALREADY_EXISTS: &str = "Session already exists";

/// The `_tag` of opencode's error for a session id its store does not hold.
const SESSION_NOT_FOUND: &str = "SessionNotFoundError";

struct Session {
    id: String,
    parent: Option<String>,
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
    let out = bounded::output(&mut command, &format!("opencode session {verb}"), TRANSFER)?;
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

pub(in crate::harness) fn unreachable(cfg: &OpenCodeConfig) -> Option<String> {
    installed_version(cfg, Probe::Fresh).err()
}

pub(in crate::harness) fn has_sessions(cfg: &OpenCodeConfig, dir: &Path) -> Result<bool> {
    Ok(!list(cfg, dir)?.is_empty())
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

pub(super) fn create_session(
    cfg: &OpenCodeConfig,
    ctx: &LaunchContext<'_>,
    spec: &SpawnSpec<'_>,
    model: &ModelRef<'_>,
) -> Result<String> {
    // The TUI reports its location by the resolved cwd.
    let directory = ctx
        .worktree
        .canonicalize()
        .unwrap_or_else(|_| ctx.worktree.to_path_buf());
    let mut body = json!({
        "location": {"directory": directory},
        "model": model.to_json(),
    });
    if let Some(name) = spec.session_name {
        body["title"] = json!(name);
    }
    if let Some(def) = spec.definition {
        body["agent"] = json!(def);
    }
    let body = body.to_string();
    session_id(&api(cfg, &["session.create", "--data", &body])?)
}

/// Pin `model` on `session`; false when opencode has no such session.
pub(super) fn pin_model(cfg: &OpenCodeConfig, session: &str, model: &ModelRef<'_>) -> Result<bool> {
    let param = format!("sessionID={session}");
    let body = json!({"model": model.to_json()}).to_string();
    let args = ["session.switchModel", "--param", &param, "--data", &body];
    match try_api(command(cfg, &["api"], &args), &args)? {
        Ok(_) => Ok(true),
        Err(refusal) if refusal.tag.as_deref() == Some(SESSION_NOT_FOUND) => Ok(false),
        Err(refusal) => Err(PmError::Agent(format!(
            "opencode session.switchModel failed: {}",
            refusal.message
        ))),
    }
}

pub(super) fn fork_session(cfg: &OpenCodeConfig, source: &str) -> Result<String> {
    let param = format!("sessionID={source}");
    session_id(&api(
        cfg,
        &["session.fork", "--param", &param, "--data", "{}"],
    )?)
}

fn session_id(response: &Value) -> Result<String> {
    response
        .pointer("/data/id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| PmError::Agent(format!("opencode returned no session id: {response}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{fake_opencode, fake_opencode_calls, fake_opencode_sequence};

    pub(super) fn cfg(binary: String) -> OpenCodeConfig {
        OpenCodeConfig {
            binary: Some(binary),
            ..Default::default()
        }
    }

    pub(super) fn page(ids: &[&str], next: Option<&str>) -> String {
        let data: Vec<Value> = ids.iter().map(|id| json!({"id": id})).collect();
        json!({"data": data, "cursor": {"previous": null, "next": next}}).to_string()
    }

    /// The value of `--param <name>=…` in a recorded call.
    pub(super) fn param<'a>(call: &'a [String], name: &str) -> Option<&'a str> {
        call.windows(2)
            .filter(|pair| pair[0] == "--param")
            .find_map(|pair| pair[1].strip_prefix(name)?.strip_prefix('='))
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
}
