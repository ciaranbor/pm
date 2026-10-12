//! Moving messages from queues keyed by bare sender name, as inboxes were
//! before queues were keyed by address, into the queue each one's metadata
//! addresses ([`address::queue_key`]). `pm upgrade` runs it on every
//! project.
//!
//! A queue's messages keep their order and its read ones stay read: each
//! address gets the legacy messages first, renumbered from 1, then any it
//! already held, and a cursor covering what was read of both. When some
//! legacy messages were unread but the address's own queue had been read
//! into, that cursor stops at the first unread legacy message, so some
//! already-read messages come again rather than any going unread. A message
//! with no metadata, or one whose address is still its bare name, stays in a
//! bare queue. Nothing is moved from a queue whose key already has an `@`,
//! so a second run finds nothing to do, beyond finishing what an
//! interrupted one left: the messages of a working copy it left go to the
//! end of their queue, unread.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::address;
use super::cursor::{cursor_path, load_cursor, save_cursor};
use super::layout::{inbox_dir, list_senders, max_index, meta_dir, remove_dir, sender_dir};
use super::read::load_meta;
use crate::error::Result;

/// Rekey every legacy queue under `messages_dir`. Returns how many
/// messages moved.
pub fn rekey(messages_dir: &Path) -> Result<usize> {
    let mut moved = 0;
    for (scope, agent) in inboxes(messages_dir)? {
        moved += recover(messages_dir, &scope, &agent)?;
        for (legacy, plan) in plans(messages_dir, &scope, &agent)? {
            moved += plan.values().map(Vec::len).sum::<usize>();
            apply(messages_dir, &scope, &agent, &legacy, plan)?;
        }
    }
    Ok(moved)
}

/// How many messages [`rekey`] would move.
pub fn rekey_pending(messages_dir: &Path) -> Result<usize> {
    let mut pending = 0;
    for (scope, agent) in inboxes(messages_dir)? {
        for key in stranded(messages_dir, &scope, &agent)? {
            let stage = format!(".rekey-{key}");
            pending += list_count(&sender_dir(messages_dir, &scope, &agent, &stage))?;
        }
        for (_, plan) in plans(messages_dir, &scope, &agent)? {
            pending += plan.values().map(Vec::len).sum::<usize>();
        }
    }
    Ok(pending)
}

/// Every `(scope, agent)` inbox under `messages_dir`.
fn inboxes(messages_dir: &Path) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for scope in subdirs(messages_dir)? {
        for agent in subdirs(&messages_dir.join(&scope))? {
            out.push((scope.clone(), agent));
        }
    }
    Ok(out)
}

fn subdirs(dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(out);
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && !name.starts_with('.') {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

/// The legacy indices of each address, in order.
type Plan = BTreeMap<String, Vec<u32>>;

/// For each legacy queue of an inbox that holds a message addressed
/// elsewhere, where each of its messages goes.
fn plans(messages_dir: &Path, scope: &str, agent: &str) -> Result<Vec<(String, Plan)>> {
    let mut out = Vec::new();
    for legacy in list_senders(&inbox_dir(messages_dir, scope, agent))? {
        if legacy.contains('@') {
            continue;
        }
        let dir = sender_dir(messages_dir, scope, agent, &legacy);
        let mut plan = Plan::new();
        for i in 1..=max_index(&dir)? {
            if !dir.join(file(i, "md")).exists() {
                continue;
            }
            let key = load_meta(messages_dir, scope, agent, &legacy, i)
                .ok()
                .flatten()
                .map_or_else(|| legacy.clone(), |m| address::queue_key(&m, scope));
            plan.entry(key).or_default().push(i);
        }
        if plan.keys().any(|key| *key != legacy) {
            out.push((legacy, plan));
        }
    }
    Ok(out)
}

fn file(index: u32, ext: &str) -> String {
    format!("{index:03}.{ext}")
}

/// Move one legacy queue's messages to the queues `plan` names: each
/// address's whole queue is built under a dot key, which no reader lists,
/// then moved back into the queue's own dirs. Every queue touched is held
/// under the lock its sends take, and its dirs and lock file stay in place,
/// so a send to one waits and lands after the moved messages.
fn apply(messages_dir: &Path, scope: &str, agent: &str, legacy: &str, plan: Plan) -> Result<()> {
    let body = |key: &str| sender_dir(messages_dir, scope, agent, key);
    let meta = |key: &str| meta_dir(messages_dir, scope, agent, key);
    let keys: BTreeSet<&str> = std::iter::once(legacy)
        .chain(plan.keys().map(String::as_str))
        .collect();
    let mut locks = Vec::new();
    for key in &keys {
        locks.push(lock(messages_dir, scope, agent, key)?);
    }

    let cpath = cursor_path(messages_dir, scope, agent);
    let mut cursor = load_cursor(&cpath)?;
    let legacy_read = cursor.remove(legacy).unwrap_or(0);
    for (key, legacy_indices) in &plan {
        let stage = format!(".rekey-{key}");
        std::fs::create_dir_all(body(&stage))?;
        std::fs::create_dir_all(meta(&stage))?;

        let mut sources: Vec<(&str, u32)> = legacy_indices.iter().map(|&i| (legacy, i)).collect();
        let existing = if key == legacy {
            0
        } else {
            max_index(&body(key))?
        };
        sources.extend((1..=existing).map(|i| (key.as_str(), i)));
        let mut index = 0;
        for (from, i) in sources {
            let msg = body(from).join(file(i, "md"));
            if !msg.exists() {
                continue;
            }
            index += 1;
            std::fs::rename(&msg, body(&stage).join(file(index, "md")))?;
            let msg_meta = meta(from).join(file(i, "json"));
            if msg_meta.exists() {
                std::fs::rename(&msg_meta, meta(&stage).join(file(index, "json")))?;
            }
        }

        let read = legacy_indices.iter().filter(|&&i| i <= legacy_read).count() as u32;
        let key_read = if key == legacy {
            0
        } else {
            cursor.get(key).copied().unwrap_or(0)
        };
        let new_cursor = if read as usize == legacy_indices.len() {
            read + key_read
        } else {
            read
        };
        if new_cursor > 0 {
            cursor.insert(key.clone(), new_cursor);
        } else {
            cursor.remove(key);
        }
    }

    if !plan.contains_key(legacy) {
        remove_dir(&body(legacy))?;
    }
    for key in plan.keys() {
        append(messages_dir, scope, agent, &format!(".rekey-{key}"), key)?;
    }
    save_cursor(&cpath, &cursor)
}

/// Hold the lock sends to the queue `key` take.
fn lock(messages_dir: &Path, scope: &str, agent: &str, key: &str) -> Result<std::fs::File> {
    let dir = meta_dir(messages_dir, scope, agent, key);
    std::fs::create_dir_all(&dir)?;
    let lock = std::fs::File::create(dir.join(".lock"))?;
    lock.lock()?;
    Ok(lock)
}

/// Move every message of queue `from`, in order, to the end of queue `to`,
/// then remove `from`. The caller holds `to`'s lock.
fn append(messages_dir: &Path, scope: &str, agent: &str, from: &str, to: &str) -> Result<()> {
    let (from_body, from_meta) = (
        sender_dir(messages_dir, scope, agent, from),
        meta_dir(messages_dir, scope, agent, from),
    );
    let (to_body, to_meta) = (
        sender_dir(messages_dir, scope, agent, to),
        meta_dir(messages_dir, scope, agent, to),
    );
    std::fs::create_dir_all(&to_body)?;
    std::fs::create_dir_all(&to_meta)?;
    let mut index = max_index(&to_body)?;
    for i in 1..=max_index(&from_body)? {
        let msg = from_body.join(file(i, "md"));
        if !msg.exists() {
            continue;
        }
        index += 1;
        let msg_meta = from_meta.join(file(i, "json"));
        if msg_meta.exists() {
            std::fs::rename(&msg_meta, to_meta.join(file(index, "json")))?;
        }
        std::fs::rename(&msg, to_body.join(file(index, "md")))?;
    }
    remove_dir(&from_body)?;
    remove_dir(&from_meta)
}

/// Move the messages of every working copy an interrupted run left in an
/// inbox to the end of the queue it was building. They count as unread, so
/// some may come again; none is lost. Returns how many moved.
fn recover(messages_dir: &Path, scope: &str, agent: &str) -> Result<usize> {
    let mut moved = 0;
    for key in stranded(messages_dir, scope, agent)? {
        let stage = format!(".rekey-{key}");
        let _lock = lock(messages_dir, scope, agent, &key)?;
        moved += list_count(&sender_dir(messages_dir, scope, agent, &stage))?;
        append(messages_dir, scope, agent, &stage, &key)?;
    }
    Ok(moved)
}

/// The keys of the queues an interrupted run left working copies of.
fn stranded(messages_dir: &Path, scope: &str, agent: &str) -> Result<Vec<String>> {
    let Ok(entries) = std::fs::read_dir(inbox_dir(messages_dir, scope, agent)) else {
        return Ok(Vec::new());
    };
    let mut keys = Vec::new();
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(key) = name.strip_prefix("from-.rekey-") {
            keys.push(key.to_string());
        }
    }
    Ok(keys)
}

fn list_count(dir: &Path) -> Result<usize> {
    Ok((1..=max_index(dir)?)
        .filter(|&i| dir.join(file(i, "md")).exists())
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{MessageMeta, check, cursor_for, list, read_at, send_meta};
    use chrono::Utc;
    use tempfile::tempdir;

    /// Write a message the way an inbox keyed by bare name held it.
    fn legacy_send(mdir: &Path, scope: &str, agent: &str, meta: Option<MessageMeta>, body: &str) {
        let sender = meta
            .as_ref()
            .map_or("user".to_string(), |m| m.sender.clone());
        let dir = sender_dir(mdir, scope, agent, &sender);
        std::fs::create_dir_all(&dir).unwrap();
        let index = max_index(&dir).unwrap() + 1;
        std::fs::write(dir.join(file(index, "md")), body).unwrap();
        if let Some(meta) = meta {
            let mdir = meta_dir(mdir, scope, agent, &sender);
            std::fs::create_dir_all(&mdir).unwrap();
            std::fs::write(
                mdir.join(file(index, "json")),
                serde_json::to_string(&meta).unwrap(),
            )
            .unwrap();
        }
    }

    fn meta(sender: &str, scope: Option<&str>) -> MessageMeta {
        MessageMeta {
            sender: sender.to_string(),
            timestamp: Utc::now(),
            sender_scope: scope.map(str::to_string),
            sender_project: None,
            from_outside: false,
        }
    }

    fn bodies(mdir: &Path, sender: &str) -> Vec<String> {
        list(mdir, "main", "main", Some(sender))
            .unwrap()
            .into_iter()
            .filter(|s| s.sender == sender)
            .map(|s| {
                read_at(mdir, "main", "main", sender, s.index)
                    .unwrap()
                    .unwrap()
                    .body
            })
            .collect()
    }

    #[test]
    fn a_shared_queue_splits_by_address_keeping_order_and_what_was_read() {
        let dir = tempdir().unwrap();
        let mdir = dir.path();
        legacy_send(
            mdir,
            "main",
            "main",
            Some(meta("implementer", Some("login"))),
            "l1",
        );
        legacy_send(
            mdir,
            "main",
            "main",
            Some(meta("implementer", Some("signup"))),
            "s1",
        );
        legacy_send(
            mdir,
            "main",
            "main",
            Some(meta("implementer", Some("login"))),
            "l2",
        );
        legacy_send(
            mdir,
            "main",
            "main",
            Some(meta("implementer", Some("signup"))),
            "s2",
        );
        let mut cursor = crate::messages::types::Cursor::new();
        cursor.insert("implementer".into(), 2);
        save_cursor(&cursor_path(mdir, "main", "main"), &cursor).unwrap();

        assert_eq!(rekey_pending(mdir).unwrap(), 4);
        assert_eq!(rekey(mdir).unwrap(), 4);

        assert_eq!(bodies(mdir, "implementer@login"), ["l1", "l2"]);
        assert_eq!(bodies(mdir, "implementer@signup"), ["s1", "s2"]);
        assert_eq!(
            cursor_for(mdir, "main", "main", "implementer@login").unwrap(),
            1
        );
        assert_eq!(
            cursor_for(mdir, "main", "main", "implementer@signup").unwrap(),
            1
        );
        let unread: u32 = check(mdir, "main", "main")
            .unwrap()
            .iter()
            .map(|s| s.count)
            .sum();
        assert_eq!(unread, 2);
        assert!(!mdir.join("main/main/from-implementer").exists());
        assert_eq!(rekey_pending(mdir).unwrap(), 0);
    }

    #[test]
    fn legacy_messages_go_before_ones_already_at_the_new_address() {
        let dir = tempdir().unwrap();
        let mdir = dir.path();
        legacy_send(
            mdir,
            "main",
            "main",
            Some(meta("implementer", Some("login"))),
            "old",
        );
        let mut cursor = crate::messages::types::Cursor::new();
        cursor.insert("implementer".into(), 1);
        save_cursor(&cursor_path(mdir, "main", "main"), &cursor).unwrap();
        send_meta(
            mdir,
            "main",
            "main",
            &meta("implementer", Some("login")),
            "new",
        )
        .unwrap();

        rekey(mdir).unwrap();

        assert_eq!(bodies(mdir, "implementer@login"), ["old", "new"]);
        assert_eq!(
            cursor_for(mdir, "main", "main", "implementer@login").unwrap(),
            1
        );
    }

    #[test]
    fn messages_with_no_address_stay_in_a_bare_queue() {
        let dir = tempdir().unwrap();
        let mdir = dir.path();
        legacy_send(mdir, "main", "main", None, "undated");
        legacy_send(
            mdir,
            "main",
            "main",
            Some(meta("no-reply-brief", Some("main"))),
            "brief",
        );

        assert_eq!(rekey_pending(mdir).unwrap(), 0);
        assert_eq!(rekey(mdir).unwrap(), 0);
        assert_eq!(bodies(mdir, "user"), ["undated"]);
        assert_eq!(bodies(mdir, "no-reply-brief"), ["brief"]);
    }

    #[test]
    fn a_working_copy_left_by_an_interrupted_run_is_no_sender_until_recovered() {
        let dir = tempdir().unwrap();
        let mdir = dir.path();
        send_meta(
            mdir,
            "main",
            "main",
            &meta("implementer", Some("login")),
            "hi",
        )
        .unwrap();
        let stage = sender_dir(mdir, "main", "main", ".rekey-implementer@login");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join(file(1, "md")), "copy").unwrap();

        let senders: Vec<String> = check(mdir, "main", "main")
            .unwrap()
            .into_iter()
            .map(|s| s.sender)
            .collect();
        assert_eq!(senders, ["implementer@login"]);
        assert_eq!(list(mdir, "main", "main", None).unwrap().len(), 1);

        assert_eq!(rekey_pending(mdir).unwrap(), 1);
        assert_eq!(rekey(mdir).unwrap(), 1);
        assert_eq!(bodies(mdir, "implementer@login"), ["hi", "copy"]);
        assert!(!stage.exists());
        assert_eq!(rekey_pending(mdir).unwrap(), 0);
    }
}
