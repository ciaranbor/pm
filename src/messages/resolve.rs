//! Which sender a read takes next: the one `--from` names, or else the
//! sender whose earliest unread message is oldest.

use std::path::Path;

use chrono::Utc;

use super::address;
use super::cursor;
use super::layout::{inbox_dir, list_senders};
use super::read::{check, load_meta};
use super::types::SenderChoice;
use crate::error::Result;

/// The sender address `from` names in an agent's inbox
/// ([`address::resolve`]).
pub fn resolve_from(messages_dir: &Path, feature: &str, agent: &str, from: &str) -> Result<String> {
    let keys = list_senders(&inbox_dir(messages_dir, feature, agent))?;
    address::resolve(keys.iter().map(String::as_str), from)
}

/// Resolve which sender a read operates on: the one `from` names when
/// given ([`resolve_from`]), otherwise the sender whose earliest unread
/// message is oldest. `pending` lists every
/// other sender with unread messages, oldest first. `None` only when nothing
/// is unread and no `from` was given.
///
/// A sender's age is the timestamp of its earliest unread message. Missing
/// or unreadable metadata (a legacy message) counts as undated and sorts
/// first, so this never fails on a message that `check` reports.
pub fn resolve_sender(
    messages_dir: &Path,
    feature: &str,
    agent: &str,
    from: Option<&str>,
) -> Result<Option<SenderChoice>> {
    let from = from
        .map(|from| resolve_from(messages_dir, feature, agent, from))
        .transpose()?;

    let csr = cursor::load_cursor(&cursor::cursor_path(messages_dir, feature, agent))?;
    let mut dated: Vec<(Option<chrono::DateTime<Utc>>, String)> =
        check(messages_dir, feature, agent)?
            .into_iter()
            .map(|s| {
                let next = csr.get(s.sender.as_str()).copied().unwrap_or(0) + 1;
                let sent = load_meta(messages_dir, feature, agent, &s.sender, next)
                    .ok()
                    .flatten()
                    .map(|m| m.timestamp);
                (sent, s.sender)
            })
            .collect();
    dated.sort();
    let mut senders: Vec<String> = dated.into_iter().map(|(_, sender)| sender).collect();

    let sender = match from {
        Some(s) => {
            senders.retain(|name| *name != s);
            s
        }
        None if senders.is_empty() => return Ok(None),
        None => senders.remove(0),
    };
    Ok(Some(SenderChoice {
        sender,
        pending: senders,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::layout::meta_dir;
    use crate::messages::test_support::messages_dir;
    use crate::messages::{next, send};
    use tempfile::tempdir;

    fn choice(sender: &str, pending: &[&str]) -> SenderChoice {
        SenderChoice {
            sender: sender.to_string(),
            pending: pending.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn resolve_sender_explicit_passthrough() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let r = resolve_sender(&mdir, "login", "reviewer", Some("implementer")).unwrap();
        assert_eq!(r, Some(choice("implementer", &[])));
    }

    #[test]
    fn resolve_sender_explicit_lists_other_unread_senders_as_pending() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        send(&mdir, "login", "reviewer", "user", "b").unwrap();

        let r = resolve_sender(&mdir, "login", "reviewer", Some("user")).unwrap();
        assert_eq!(r, Some(choice("user@login", &["implementer@login"])));
    }

    #[test]
    fn resolve_sender_implicit_when_one_unread() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();
        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, Some(choice("implementer@login", &[])));
    }

    #[test]
    fn resolve_sender_no_unread_when_empty() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, None);
    }

    #[test]
    fn resolve_sender_no_unread_after_next() {
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();
        next(&mdir, "login", "reviewer", "implementer@login").unwrap();

        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, None);
    }

    #[test]
    fn resolve_sender_picks_oldest_unread_sender_and_orders_pending_by_age() {
        // Queued out of name order: "zed" first, then "amy", then "mid".
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        for sender in ["zed", "amy", "mid"] {
            send(&mdir, "login", "reviewer", sender, "hi").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, Some(choice("zed@login", &["amy@login", "mid@login"])));
    }

    #[test]
    fn resolve_sender_ages_by_earliest_unread_not_latest() {
        // "amy" sent first but that message was read; her remaining unread
        // is newer than "zed"'s, so zed is oldest.
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "amy", "read already").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        send(&mdir, "login", "reviewer", "zed", "unread").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        send(&mdir, "login", "reviewer", "amy", "unread").unwrap();
        next(&mdir, "login", "reviewer", "amy@login").unwrap();

        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, Some(choice("zed@login", &["amy@login"])));
    }

    #[test]
    fn resolve_sender_tolerates_unreadable_meta() {
        // A meta file that does not parse (empty or corrupt) must count as
        // undated, not fail the read or the hook.
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "amy", "dated").unwrap();
        send(&mdir, "login", "reviewer", "zed", "undated").unwrap();
        std::fs::write(
            meta_dir(&mdir, "login", "reviewer", "zed@login").join("001.json"),
            "",
        )
        .unwrap();

        assert_eq!(check(&mdir, "login", "reviewer").unwrap().len(), 2);
        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, Some(choice("zed@login", &["amy@login"])));
    }

    #[test]
    fn resolve_sender_implicit_picks_the_unread_one() {
        // Two senders exist, but only one has unread. Implicit resolution
        // picks that sender with nothing pending.
        let dir = tempdir().unwrap();
        let mdir = messages_dir(dir.path());

        send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        send(&mdir, "login", "reviewer", "user", "b").unwrap();
        next(&mdir, "login", "reviewer", "implementer@login").unwrap();

        let r = resolve_sender(&mdir, "login", "reviewer", None).unwrap();
        assert_eq!(r, Some(choice("user@login", &[])));
    }
}
