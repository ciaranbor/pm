//! **Scheduled sends**: messages `pm msg send --in` holds until they fall
//! due, one file each in `<project>/.pm/runtime/.scheduled/`, in the
//! recipient's project. Like all runtime state they never leave the
//! machine, so no other machine can deliver them a second time.
//!
//! Several waiters sweep the store at once
//! ([`msg_scheduled`](crate::commands::msg_scheduled)): one claims a due
//! send by renaming `<id>.json` to `<id>.json.<pid>`, and the others find
//! it gone. A claim whose process died is unclaimed again.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{created, root};
use crate::error::{PmError, Result};
use crate::fs_utils::{process_alive, write_atomic};

const DIR: &str = ".scheduled";
const EXT: &str = ".json";

/// A message held until `due`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scheduled {
    pub id: String,
    pub due: DateTime<Utc>,
    pub created: DateTime<Utc>,
    /// The recipient's scope.
    pub scope: String,
    pub recipient: String,
    pub sender: String,
    /// Where a reply reaches the sender, if anywhere.
    pub home: Option<ScheduledHome>,
    pub body: String,
    /// Why delivery failed for good; such a send is never retried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// When a delivery first failed and gave the send back for a later
    /// sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrying_since: Option<DateTime<Utc>>,
}

/// A sender's home, as [`Home`](crate::commands::agent_send::Home) owned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledHome {
    pub root: PathBuf,
    pub scope: String,
}

/// A due send this process claimed: deliver it, then [`finish`], [`fail`]
/// or [`retry`] it.
#[derive(Debug)]
pub struct Claimed {
    pub send: Scheduled,
    path: PathBuf,
}

fn dir(project_root: &Path) -> PathBuf {
    root(project_root).join(DIR)
}

fn fresh_id() -> String {
    let mut bytes = [0u8; 3];
    match getrandom::fill(&mut bytes) {
        Ok(()) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        Err(_) => format!("{:06x}", Utc::now().timestamp_micros() & 0xff_ffff),
    }
}

/// Store `send` under a fresh id, which it returns with.
pub fn schedule(project_root: &Path, mut send: Scheduled) -> Result<Scheduled> {
    let dir = created(dir(project_root), project_root)?;
    send.id = loop {
        let id = fresh_id();
        if !std::fs::exists(dir.join(format!("{id}{EXT}")))? {
            break id;
        }
    };
    write_atomic(
        &dir.join(format!("{}{EXT}", send.id)),
        &serde_json::to_vec(&send)?,
    )?;
    Ok(send)
}

/// Each stored file: its path, its record, and the pid of the process that
/// claimed it, if one did. Unreadable files are skipped.
fn entries(project_root: &Path) -> Result<Vec<(PathBuf, Scheduled, Option<u32>)>> {
    let read = match std::fs::read_dir(dir(project_root)) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut entries = Vec::new();
    for entry in read {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let claimer = match name.split_once(&format!("{EXT}.")) {
            Some((_, pid)) => match pid.parse() {
                Ok(pid) => Some(pid),
                Err(_) => continue,
            },
            None if name.ends_with(EXT) => None,
            None => continue,
        };
        let Some(send) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        else {
            continue;
        };
        entries.push((path, send, claimer));
    }
    Ok(entries)
}

/// Every stored send, soonest due first.
pub fn list(project_root: &Path) -> Result<Vec<Scheduled>> {
    let mut sends: Vec<Scheduled> = entries(project_root)?
        .into_iter()
        .map(|(_, send, _)| send)
        .collect();
    sends.sort_by_key(|s| s.due);
    Ok(sends)
}

/// Remove the send whose id starts with `prefix`, unless a live process
/// is delivering it.
pub fn remove(project_root: &Path, prefix: &str) -> Result<Scheduled> {
    let mut matches: Vec<_> = entries(project_root)?
        .into_iter()
        .filter(|(_, send, _)| !prefix.is_empty() && send.id.starts_with(prefix))
        .collect();
    let (path, send, claimer) = match matches.len() {
        0 => {
            return Err(PmError::Messaging(format!(
                "No scheduled send with id '{prefix}' (`pm msg scheduled` lists them)"
            )));
        }
        1 => matches.remove(0),
        _ => {
            let ids: Vec<_> = matches.iter().map(|(_, s, _)| s.id.as_str()).collect();
            return Err(PmError::Messaging(format!(
                "Id '{prefix}' matches several scheduled sends: {}",
                ids.join(", ")
            )));
        }
    };
    let delivering =
        || PmError::Messaging(format!("Scheduled send '{}' is being delivered", send.id));
    if claimer.is_some_and(process_alive) {
        return Err(delivering());
    }
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(delivering()),
        other => {
            other?;
            Ok(send)
        }
    }
}

/// Claim every send due by `now` that has not failed, soonest due first:
/// unclaimed ones, and those whose claimer died.
pub fn claim_due(project_root: &Path, now: DateTime<Utc>) -> Result<Vec<Claimed>> {
    let own = std::process::id();
    let mut claimed = Vec::new();
    for (path, send, claimer) in entries(project_root)? {
        if send.due > now
            || send.failed.is_some()
            || claimer.is_some_and(|pid| pid == own || process_alive(pid))
        {
            continue;
        }
        let ours = dir(project_root).join(format!("{}{EXT}.{own}", send.id));
        match std::fs::rename(&path, &ours) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            other => other?,
        }
        // Read again: another sweeper may have failed it since.
        let send = match std::fs::read(&ours)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Scheduled>(&bytes).ok())
        {
            Some(send) if send.failed.is_none() => send,
            _ => {
                std::fs::rename(&ours, &path)?;
                continue;
            }
        };
        claimed.push(Claimed { send, path: ours });
    }
    claimed.sort_by_key(|c| c.send.due);
    Ok(claimed)
}

fn unclaimed_path(claimed: &Claimed) -> PathBuf {
    claimed
        .path
        .with_file_name(format!("{}{EXT}", claimed.send.id))
}

/// Give `claimed`, whose delivery failed at `now`, back for a later sweep
/// to retry.
pub fn retry(claimed: Claimed, now: DateTime<Utc>) -> Result<()> {
    let send = Scheduled {
        retrying_since: claimed.send.retrying_since.or(Some(now)),
        ..claimed.send.clone()
    };
    write_atomic(&unclaimed_path(&claimed), &serde_json::to_vec(&send)?)?;
    Ok(std::fs::remove_file(claimed.path)?)
}

/// Drop `claimed`: it was delivered.
pub fn finish(claimed: Claimed) -> Result<()> {
    Ok(std::fs::remove_file(claimed.path)?)
}

/// Keep `claimed` as failed for good, saying `why`.
pub fn fail(claimed: Claimed, why: String) -> Result<()> {
    let send = Scheduled {
        failed: Some(why),
        ..claimed.send.clone()
    };
    write_atomic(&unclaimed_path(&claimed), &serde_json::to_vec(&send)?)?;
    Ok(std::fs::remove_file(claimed.path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn send(due: DateTime<Utc>) -> Scheduled {
        Scheduled {
            id: String::new(),
            due,
            created: Utc::now(),
            scope: "login".into(),
            recipient: "reviewer".into(),
            sender: "implementer".into(),
            home: None,
            body: "hi".into(),
            failed: None,
            retrying_since: None,
        }
    }

    #[test]
    fn a_send_is_claimed_once_due_and_by_one_claimer() {
        let dir = tempdir().unwrap();
        let now = Utc::now();
        let stored = schedule(dir.path(), send(now + chrono::Duration::minutes(5))).unwrap();

        assert!(claim_due(dir.path(), now).unwrap().is_empty());
        let later = now + chrono::Duration::minutes(6);
        let claimed = claim_due(dir.path(), later).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].send.id, stored.id);
        assert!(claim_due(dir.path(), later).unwrap().is_empty());

        retry(claimed.into_iter().next().unwrap(), later).unwrap();
        let claimed = claim_due(dir.path(), later).unwrap();
        assert_eq!(claimed[0].send.retrying_since, Some(later));
    }

    #[test]
    fn a_claim_whose_process_died_is_claimed_again() {
        let dir = tempdir().unwrap();
        let now = Utc::now();
        let stored = schedule(dir.path(), send(now)).unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let store = super::dir(dir.path());
        std::fs::rename(
            store.join(format!("{}.json", stored.id)),
            store.join(format!("{}.json.{dead}", stored.id)),
        )
        .unwrap();

        let claimed = claim_due(dir.path(), now).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].send.id, stored.id);
    }

    #[test]
    fn a_failed_send_is_listed_but_never_claimed() {
        let dir = tempdir().unwrap();
        let now = Utc::now();
        schedule(dir.path(), send(now)).unwrap();
        let claimed = claim_due(dir.path(), now).unwrap().remove(0);
        fail(claimed, "gone".into()).unwrap();

        assert!(claim_due(dir.path(), now).unwrap().is_empty());
        let listed = list(dir.path()).unwrap();
        assert_eq!(listed[0].failed.as_deref(), Some("gone"));
    }

    /// Store a send due `now` under `id`.
    fn put(project_root: &Path, id: &str, now: DateTime<Utc>) -> Scheduled {
        let stored = Scheduled {
            id: id.into(),
            ..send(now)
        };
        let store = created(super::dir(project_root), project_root).unwrap();
        let bytes = serde_json::to_vec(&stored).unwrap();
        write_atomic(&store.join(format!("{id}.json")), &bytes).unwrap();
        stored
    }

    #[test]
    fn remove_takes_a_unique_prefix() {
        let dir = tempdir().unwrap();
        let now = Utc::now();
        let a = put(dir.path(), "abc123", now);
        let b = put(dir.path(), "def456", now);

        assert!(remove(dir.path(), "").is_err());
        assert_eq!(remove(dir.path(), "ab").unwrap(), a);
        assert_eq!(list(dir.path()).unwrap(), [b]);
    }

    #[test]
    fn a_send_being_delivered_cannot_be_cancelled_but_a_dead_claim_can() {
        let dir = tempdir().unwrap();
        let now = Utc::now();
        let stored = put(dir.path(), "abc123", now);
        let claimed = claim_due(dir.path(), now).unwrap().remove(0);

        let err = remove(dir.path(), "abc").unwrap_err().to_string();
        assert!(err.contains("being delivered"), "{err}");

        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let stale = claimed.path.with_file_name(format!("abc123.json.{dead}"));
        std::fs::rename(&claimed.path, &stale).unwrap();
        assert_eq!(remove(dir.path(), "abc").unwrap(), stored);
        assert!(list(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn an_ambiguous_prefix_removes_nothing() {
        let dir = tempdir().unwrap();
        let now = Utc::now();
        put(dir.path(), "abc123", now);
        put(dir.path(), "abd456", now);

        let err = remove(dir.path(), "ab").unwrap_err().to_string();
        assert!(err.contains("abc123") && err.contains("abd456"), "{err}");
        assert_eq!(list(dir.path()).unwrap().len(), 2);
    }
}
