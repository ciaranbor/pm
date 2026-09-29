//! Codex keeps each session as one rollout file,
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<session id>.jsonl`,
//! and finds a session by id whichever directory it was recorded in: a
//! rollout copied into a store is indexed by codex itself on first use.
//!
//! The rollout is the source of truth; codex's sqlite files are derived from
//! it and hold byte offsets into it, so a rollout is only ever copied whole —
//! never rewritten, and the databases never touched. The directory a session
//! belongs to is the `cwd` of its last `turn_context` line (`session_meta`
//! holds the one it was created in).

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::Result;
use crate::harness::{ImportOutcome, per_session_outcome};

pub(in crate::harness) const MIGRATE_NOTE: &str = "Nothing to migrate: codex finds a session \
    by id, and pm resumes it in the agent's current directory";

const SESSIONS_DIR: &str = "sessions";
const SESSION_ID_LEN: usize = 36;

/// Every rollout file under `root`, as paths relative to it.
fn rollouts(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else if session_id(&path).is_some()
                && let Ok(rel) = path.strip_prefix(root)
            {
                out.push(rel.to_path_buf());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if root.is_dir() {
        walk(root, root, &mut out)?;
    }
    out.sort();
    Ok(out)
}

/// The session id a rollout file is named for; `None` for any other file.
fn session_id(path: &Path) -> Option<&str> {
    let stem = path
        .file_name()?
        .to_str()?
        .strip_prefix("rollout-")?
        .strip_suffix(".jsonl")?;
    let at = stem.len().checked_sub(SESSION_ID_LEN)?;
    (stem.is_char_boundary(at) && stem[..at].ends_with('-')).then(|| &stem[at..])
}

/// The directory the session in `rollout` was last run in.
fn recorded_cwd(rollout: &Path) -> Result<Option<String>> {
    let mut created_in = None;
    let mut last_turn = None;
    for line in BufReader::new(std::fs::File::open(rollout)?).lines() {
        let line = line?;
        if !line.contains("\"cwd\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(cwd) = value.pointer("/payload/cwd").and_then(Value::as_str) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("turn_context") => last_turn = Some(cwd.to_string()),
            Some("session_meta") => created_in = Some(cwd.to_string()),
            _ => {}
        }
    }
    Ok(last_turn.or(created_in))
}

/// Copy the rollouts of the sessions last run in `dir` into `staging`,
/// keeping their path below `sessions/`.
pub(in crate::harness) fn export(
    codex_home: &Path,
    dir: &Path,
    staging: &Path,
) -> Result<Option<String>> {
    // Codex records the resolved cwd; pm's main worktree may be a symlink.
    let mut wanted = vec![dir.to_string_lossy().into_owned()];
    if let Ok(resolved) = dir.canonicalize() {
        wanted.push(resolved.to_string_lossy().into_owned());
    }

    let store = codex_home.join(SESSIONS_DIR);
    let mut exported = 0;
    for rel in rollouts(&store)? {
        let source = store.join(&rel);
        if !recorded_cwd(&source)?.is_some_and(|cwd| wanted.contains(&cwd)) {
            continue;
        }
        copy_into(&source, &staging.join(&rel))?;
        exported += 1;
    }
    Ok((exported > 0).then(|| format!("{exported} session(s)")))
}

/// Copy the rollouts in `staging` into the store, at the path they were
/// exported from. A session the store already has, at any path, is left as
/// it is.
pub(in crate::harness) fn import(codex_home: &Path, staging: &Path) -> Result<ImportOutcome> {
    let store = codex_home.join(SESSIONS_DIR);
    let present: HashSet<String> = rollouts(&store)?
        .iter()
        .filter_map(|rel| session_id(rel).map(str::to_string))
        .collect();

    let incoming = rollouts(staging)?;
    let mut imported = 0;
    for rel in &incoming {
        if session_id(rel).is_some_and(|id| present.contains(id)) {
            continue;
        }
        copy_into(&staging.join(rel), &store.join(rel))?;
        imported += 1;
    }
    Ok(per_session_outcome(imported, incoming.len()))
}

fn copy_into(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "01a0e810-e3f9-7fa1-a479-e6eccfd79a9c";
    const ID_B: &str = "01a0e810-e3f9-7fa1-a479-000000000002";
    const ID_C: &str = "01a0e810-e3f9-7fa1-a479-000000000003";

    fn rel(id: &str) -> PathBuf {
        PathBuf::from(format!("2026/09/28/rollout-2026-09-28T13-50-16-{id}.jsonl"))
    }

    /// A rollout created in `created_in` whose turns ran in `turns`.
    fn rollout(id: &str, created_in: &str, turns: &[&str]) -> String {
        let mut lines = vec![
            serde_json::json!({
                "type": "session_meta",
                "payload": {"id": id, "cwd": created_in},
            })
            .to_string(),
        ];
        for cwd in turns {
            lines.push(
                serde_json::json!({"type": "turn_context", "payload": {"cwd": cwd}}).to_string(),
            );
            // A tool call naming another directory is not where the turn ran.
            lines.push(
                serde_json::json!({
                    "type": "response_item",
                    "payload": {"cwd": "/somewhere/else"},
                })
                .to_string(),
            );
        }
        lines.join("\n") + "\n"
    }

    fn store_rollout(codex_home: &Path, id: &str, content: &str) -> PathBuf {
        let path = codex_home.join(SESSIONS_DIR).join(rel(id));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn export_selects_sessions_by_the_directory_of_their_last_turn() {
        let home = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        // Started elsewhere, last run in the project.
        store_rollout(
            home.path(),
            ID_A,
            &rollout(ID_A, "/old", &["/old", "/proj"]),
        );
        // Started in the project, since moved away.
        store_rollout(home.path(), ID_B, &rollout(ID_B, "/proj", &["/other"]));
        // No turn yet: where it was created counts.
        store_rollout(home.path(), ID_C, &rollout(ID_C, "/proj", &[]));
        std::fs::write(home.path().join(SESSIONS_DIR).join("notes.txt"), "x").unwrap();

        let out = staging.path().join("out");
        let detail = export(home.path(), Path::new("/proj"), &out).unwrap();
        assert_eq!(detail.as_deref(), Some("2 session(s)"));
        assert_eq!(rollouts(&out).unwrap(), vec![rel(ID_C), rel(ID_A)]);
    }

    #[test]
    fn export_matches_the_resolved_form_of_a_symlinked_directory() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let real = work.path().join("repo");
        std::fs::create_dir_all(&real).unwrap();
        let link = work.path().join("main");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let resolved = real.canonicalize().unwrap();
        store_rollout(
            home.path(),
            ID_A,
            &rollout(ID_A, &resolved.to_string_lossy(), &[]),
        );

        let out = work.path().join("out");
        assert!(export(home.path(), &link, &out).unwrap().is_some());
        assert_eq!(rollouts(&out).unwrap(), vec![rel(ID_A)]);
    }

    #[test]
    fn export_of_a_directory_without_sessions_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let out = staging.path().join("out");
        assert_eq!(export(home.path(), Path::new("/proj"), &out).unwrap(), None);
        store_rollout(home.path(), ID_A, &rollout(ID_A, "/other", &[]));
        assert_eq!(export(home.path(), Path::new("/proj"), &out).unwrap(), None);
        assert!(!out.exists());
    }

    #[test]
    fn a_round_trip_lands_each_rollout_at_its_path_unchanged() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let content = rollout(ID_A, "/proj", &["/proj"]);
        store_rollout(source.path(), ID_A, &content);

        export(source.path(), Path::new("/proj"), staging.path()).unwrap();
        let outcome = import(target.path(), staging.path()).unwrap();

        assert_eq!(
            outcome,
            ImportOutcome::Imported {
                detail: "1 session(s)".to_string(),
                notes: Vec::new(),
            }
        );
        assert_eq!(
            std::fs::read_to_string(target.path().join(SESSIONS_DIR).join(rel(ID_A))).unwrap(),
            content
        );
    }

    #[test]
    fn import_leaves_a_session_the_store_already_has_untouched() {
        let target = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        // Present under another date directory than the export's.
        let existing = target.path().join(SESSIONS_DIR).join(format!(
            "2026/10/01/rollout-2026-10-01T09-00-00-{ID_A}.jsonl"
        ));
        std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
        std::fs::write(&existing, "local history\n").unwrap();

        for id in [ID_A, ID_B] {
            let file = staging.path().join(rel(id));
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, rollout(id, "/proj", &[])).unwrap();
        }

        let outcome = import(target.path(), staging.path()).unwrap();
        assert_eq!(
            outcome,
            ImportOutcome::Imported {
                detail: "1 session(s), 1 already present".to_string(),
                notes: Vec::new(),
            }
        );
        assert_eq!(
            std::fs::read_to_string(&existing).unwrap(),
            "local history\n"
        );
        let store = target.path().join(SESSIONS_DIR);
        assert!(!store.join(rel(ID_A)).exists());
        assert!(store.join(rel(ID_B)).exists());

        // A second import has nothing left to add.
        assert_eq!(
            import(target.path(), staging.path()).unwrap(),
            ImportOutcome::Skipped("all 2 session(s) already exist locally".to_string())
        );
    }
}
