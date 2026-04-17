use std::path::Path;

use crate::error::{PmError, Result};
use crate::messages::{self, Message, SenderResolution};
use crate::state::paths;

/// Parsed `--index` spec from the CLI. `None` means "no --index given".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexSpec {
    /// Absolute: `--index 3` → message 3.
    Absolute(u32),
    /// Relative to the cursor: `--index +2` → cursor + 2.
    RelativePlus(u32),
    /// Relative to the cursor: `--index -1` → cursor - 1.
    RelativeMinus(u32),
}

impl IndexSpec {
    /// Parse the CLI string form: "3", "+2", "-1".
    pub fn parse(s: &str) -> Result<Self> {
        if let Some(rest) = s.strip_prefix('+') {
            let n: u32 = rest.parse().map_err(|_| {
                PmError::Messaging(format!(
                    "invalid --index '{s}': expected a number after '+'"
                ))
            })?;
            if n == 0 {
                return Err(PmError::Messaging(
                    "invalid --index '+0': use --index (no flag) to read the next message"
                        .to_string(),
                ));
            }
            Ok(IndexSpec::RelativePlus(n))
        } else if let Some(rest) = s.strip_prefix('-') {
            let n: u32 = rest.parse().map_err(|_| {
                PmError::Messaging(format!(
                    "invalid --index '{s}': expected a number after '-'"
                ))
            })?;
            if n == 0 {
                return Err(PmError::Messaging(
                    "invalid --index '-0': use a positive offset".to_string(),
                ));
            }
            Ok(IndexSpec::RelativeMinus(n))
        } else {
            let n: u32 = s.parse().map_err(|_| {
                PmError::Messaging(format!(
                    "invalid --index '{s}': expected an absolute number, '+N', or '-N'"
                ))
            })?;
            if n == 0 {
                return Err(PmError::Messaging(
                    "invalid --index '0': message indices start at 1".to_string(),
                ));
            }
            Ok(IndexSpec::Absolute(n))
        }
    }

    /// Resolve this spec to an absolute message index for the given cursor.
    ///
    /// Conventions (with cursor = "last processed index"):
    /// - `+N` addresses `cursor + N`, so `+1` is the next unread message.
    /// - `-N` addresses `cursor + 1 - N`, so `-1` is the last-processed
    ///   message (i.e. the cursor itself), `-2` is one before, etc.
    /// - `N` (absolute) addresses message N directly.
    fn resolve(self, cursor: u32) -> Result<u32> {
        match self {
            IndexSpec::Absolute(n) => Ok(n),
            IndexSpec::RelativePlus(n) => Ok(cursor + n),
            IndexSpec::RelativeMinus(n) => {
                // `-1` → cursor (last processed); `-N` → cursor + 1 - N.
                if n > cursor {
                    Err(PmError::Messaging(format!(
                        "--index -{n} goes before the start of the inbox (cursor is at {cursor})"
                    )))
                } else {
                    Ok(cursor + 1 - n)
                }
            }
        }
    }
}

/// Read a single message from an agent's inbox.
///
/// Without `--index`: reads the next unread message (cursor + 1) and
/// **advances the cursor** so that repeated calls walk through the queue.
/// This collapses the old read-then-next two-step into one command.
///
/// With `--index`: reads a specific message without touching the cursor
/// (pure historical lookup). `--from` must be explicit in this case.
///
/// Returns formatted output lines suitable for printing.
pub fn agent_read(
    project_root: &Path,
    feature: &str,
    agent: &str,
    from: Option<&str>,
    index: Option<IndexSpec>,
) -> Result<Vec<String>> {
    let messages_dir = paths::messages_dir(project_root);

    match index {
        None => {
            // Next-unread mode: resolve sender, read cursor + 1, then
            // advance the cursor so the next call returns the following
            // message. If nothing is unread (NoUnread, or --from given
            // but the explicit sender has no new messages), emit the
            // friendly "No new messages" output.
            let resolution = messages::resolve_sender(&messages_dir, feature, agent, from)?;
            if let SenderResolution::NoUnread = resolution {
                return Ok(vec!["No new messages".to_string()]);
            }
            let sender = resolution.into_sender()?;
            let cur = messages::cursor_for(&messages_dir, feature, agent, &sender)?;
            let msg = messages::read_at(&messages_dir, feature, agent, &sender, cur + 1)?;
            match msg {
                Some(m) => {
                    // Advance cursor past this message.
                    messages::next(&messages_dir, feature, agent, &sender)?;
                    Ok(format_message(&m))
                }
                None => Ok(vec![format!("No new messages from {sender}")]),
            }
        }
        Some(spec) => {
            // --index requires explicit --from: the caller is deliberately
            // addressing a historical message, not "the next unread".
            // Does NOT advance the cursor.
            let sender = from.ok_or_else(|| {
                PmError::Messaging("--index requires --from <sender>".to_string())
            })?;
            let cur = messages::cursor_for(&messages_dir, feature, agent, sender)?;
            let resolved = spec.resolve(cur)?;
            let msg = messages::read_at(&messages_dir, feature, agent, sender, resolved)?;
            match msg {
                Some(m) => Ok(format_message(&m)),
                None => Ok(vec![format!("No message [{resolved:03}] from {sender}")]),
            }
        }
    }
}

fn format_message(m: &Message) -> Vec<String> {
    let mut sender_display = match &m.meta.sender_scope {
        Some(scope) => format!("{}@{}", m.sender, scope),
        None => m.sender.clone(),
    };
    if let Some(project) = &m.meta.sender_project {
        sender_display = format!("{sender_display} (project: {project})");
    }
    vec![
        format!(
            "--- from {} [{:03}] {} ---",
            sender_display,
            m.index,
            m.meta.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
        ),
        m.body.clone(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn setup_project(dir: &Path) -> PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        root
    }

    // --- IndexSpec parsing ---

    #[test]
    fn index_spec_parse_absolute() {
        assert_eq!(IndexSpec::parse("3").unwrap(), IndexSpec::Absolute(3));
        assert_eq!(IndexSpec::parse("12").unwrap(), IndexSpec::Absolute(12));
    }

    #[test]
    fn index_spec_rejects_absolute_zero() {
        // Message indices start at 1; absolute 0 is nonsensical and should
        // be rejected at parse time, not silently resolve to "no message".
        let err = IndexSpec::parse("0").unwrap_err();
        assert!(format!("{err}").contains("indices start at 1"));
    }

    #[test]
    fn index_spec_parse_relative_plus() {
        assert_eq!(IndexSpec::parse("+2").unwrap(), IndexSpec::RelativePlus(2));
    }

    #[test]
    fn index_spec_parse_relative_minus() {
        assert_eq!(IndexSpec::parse("-1").unwrap(), IndexSpec::RelativeMinus(1));
    }

    #[test]
    fn index_spec_rejects_garbage() {
        assert!(IndexSpec::parse("abc").is_err());
        assert!(IndexSpec::parse("+abc").is_err());
        assert!(IndexSpec::parse("-").is_err());
        assert!(IndexSpec::parse("+0").is_err());
        assert!(IndexSpec::parse("-0").is_err());
    }

    #[test]
    fn index_spec_resolve_relative_minus_past_start_errors() {
        // cursor=2, -3 → 2 + 1 - 3 = 0, which is before the start.
        let err = IndexSpec::RelativeMinus(3).resolve(2).unwrap_err();
        assert!(format!("{err}").contains("before the start"));
    }

    #[test]
    fn index_spec_resolve_cursor_plus_one_minus_n() {
        // +1 is the next unread (cursor + 1).
        assert_eq!(IndexSpec::RelativePlus(1).resolve(3).unwrap(), 4);
        // -1 is the last processed (cursor).
        assert_eq!(IndexSpec::RelativeMinus(1).resolve(3).unwrap(), 3);
        // -2 is one back from the last processed.
        assert_eq!(IndexSpec::RelativeMinus(2).resolve(3).unwrap(), 2);
        // -N with N == cursor hits index 1 (the earliest message).
        assert_eq!(IndexSpec::RelativeMinus(3).resolve(3).unwrap(), 1);
    }

    // --- agent_read ---

    #[test]
    fn read_next_unread_implicit_sender() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "fix the bug").unwrap();

        let lines = agent_read(&root, "login", "reviewer", None, None).unwrap();
        assert!(lines[0].starts_with("--- from implementer [001]"));
        assert_eq!(lines[1], "fix the bug");
    }

    #[test]
    fn read_no_messages() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let lines = agent_read(&root, "login", "reviewer", None, None).unwrap();
        assert_eq!(lines, vec!["No new messages"]);
    }

    #[test]
    fn read_advances_cursor() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "two").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "three").unwrap();

        let lines = agent_read(&root, "login", "reviewer", None, None).unwrap();
        assert!(lines[1].contains("one"));
        assert_eq!(
            messages::cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            1
        );

        let lines = agent_read(&root, "login", "reviewer", None, None).unwrap();
        assert!(lines[1].contains("two"));
        assert_eq!(
            messages::cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            2
        );

        let lines = agent_read(&root, "login", "reviewer", None, None).unwrap();
        assert!(lines[1].contains("three"));
        assert_eq!(
            messages::cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            3
        );

        // After exhausting all messages, should say no new messages.
        let lines = agent_read(&root, "login", "reviewer", None, None).unwrap();
        assert_eq!(lines, vec!["No new messages"]);
    }

    #[test]
    fn read_with_index_does_not_advance_cursor() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();

        // Reading with --index is pure: cursor stays at 0.
        agent_read(
            &root,
            "login",
            "reviewer",
            Some("implementer"),
            Some(IndexSpec::Absolute(1)),
        )
        .unwrap();

        assert_eq!(
            messages::cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            0
        );
    }

    #[test]
    fn read_implicit_ambiguous_errors() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        messages::send(&mdir, "login", "reviewer", "user", "b").unwrap();

        let err = agent_read(&root, "login", "reviewer", None, None).unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("multiple senders"));
        assert!(s.contains("implementer"));
        assert!(s.contains("user"));
    }

    #[test]
    fn read_explicit_from_picks_one_sender() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        messages::send(&mdir, "login", "reviewer", "user", "b").unwrap();

        let lines = agent_read(&root, "login", "reviewer", Some("user"), None).unwrap();
        assert!(lines[0].contains("from user"));
        assert_eq!(lines[1], "b");
    }

    #[test]
    fn read_absolute_index_requires_from() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();

        let err = agent_read(
            &root,
            "login",
            "reviewer",
            None,
            Some(IndexSpec::Absolute(1)),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("--index requires --from"));
    }

    #[test]
    fn read_absolute_index_with_from() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "two").unwrap();

        let lines = agent_read(
            &root,
            "login",
            "reviewer",
            Some("implementer"),
            Some(IndexSpec::Absolute(2)),
        )
        .unwrap();
        assert!(lines[0].starts_with("--- from implementer [002]"));
        assert_eq!(lines[1], "two");
    }

    #[test]
    fn read_relative_plus_peeks_ahead() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "two").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "three").unwrap();

        // Cursor at 0. +2 → index 2.
        let lines = agent_read(
            &root,
            "login",
            "reviewer",
            Some("implementer"),
            Some(IndexSpec::RelativePlus(2)),
        )
        .unwrap();
        assert!(lines[0].starts_with("--- from implementer [002]"));
        assert_eq!(lines[1], "two");
    }

    #[test]
    fn read_relative_minus_one_is_last_processed() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "two").unwrap();
        messages::next(&mdir, "login", "reviewer", "implementer").unwrap();
        messages::next(&mdir, "login", "reviewer", "implementer").unwrap();

        // Cursor at 2 (= last processed). -1 → the last processed message,
        // i.e. index 2 ("two"). This matches the symmetry: +1 is the next
        // unread, -1 is the last processed.
        let lines = agent_read(
            &root,
            "login",
            "reviewer",
            Some("implementer"),
            Some(IndexSpec::RelativeMinus(1)),
        )
        .unwrap();
        assert!(lines[0].starts_with("--- from implementer [002]"));
        assert_eq!(lines[1], "two");
    }

    #[test]
    fn read_relative_minus_two_goes_one_further_back() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "two").unwrap();
        messages::next(&mdir, "login", "reviewer", "implementer").unwrap();
        messages::next(&mdir, "login", "reviewer", "implementer").unwrap();

        // Cursor at 2. -2 → cursor + 1 - 2 = index 1 ("one").
        let lines = agent_read(
            &root,
            "login",
            "reviewer",
            Some("implementer"),
            Some(IndexSpec::RelativeMinus(2)),
        )
        .unwrap();
        assert!(lines[0].starts_with("--- from implementer [001]"));
        assert_eq!(lines[1], "one");
    }

    #[test]
    fn read_relative_minus_one_errors_at_cursor_zero() {
        // Before anything is processed (cursor = 0), -1 has nothing to
        // look back at; the formula says cursor + 1 - 1 = 0 which is
        // rejected as "before the start".
        let err = IndexSpec::RelativeMinus(1).resolve(0).unwrap_err();
        assert!(format!("{err}").contains("before the start"));
    }

    #[test]
    fn read_nonexistent_index_reports_friendly() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "one").unwrap();

        let lines = agent_read(
            &root,
            "login",
            "reviewer",
            Some("implementer"),
            Some(IndexSpec::Absolute(99)),
        )
        .unwrap();
        assert_eq!(lines, vec!["No message [099] from implementer"]);
    }

    #[test]
    fn read_no_unread_with_explicit_from_says_no_new_messages() {
        // When --from is explicit but that sender has no unread messages,
        // we emit the friendly "No new messages from X" form, not the
        // technical "No message [NNN]" form which is reserved for --index.
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "msg").unwrap();
        messages::next(&mdir, "login", "reviewer", "implementer").unwrap();

        let lines = agent_read(&root, "login", "reviewer", Some("implementer"), None).unwrap();
        assert_eq!(lines, vec!["No new messages from implementer"]);
    }

    #[test]
    fn read_displays_sender_scope_when_present() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        // Use send_with_scope to set the sender scope
        messages::send_with_scope(
            &mdir,
            "login",
            "reviewer",
            "implementer",
            "cross-scope msg",
            Some("other-feature"),
        )
        .unwrap();

        let lines = agent_read(&root, "login", "reviewer", None, None).unwrap();
        assert!(lines[0].starts_with("--- from implementer@other-feature [001]"));
        assert_eq!(lines[1], "cross-scope msg");
    }

    #[test]
    fn read_displays_sender_project_when_present() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send_full(
            &mdir,
            "main",
            "reviewer",
            "implementer",
            "cross-project msg",
            Some("login"),
            Some("other-app"),
        )
        .unwrap();

        let lines = agent_read(&root, "main", "reviewer", None, None).unwrap();
        assert!(lines[0].contains("implementer@login (project: other-app)"));
        assert_eq!(lines[1], "cross-project msg");
    }

    #[test]
    fn read_omits_scope_when_not_set() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        // Use plain send (no scope)
        messages::send(&mdir, "login", "reviewer", "implementer", "same-scope msg").unwrap();

        let lines = agent_read(&root, "login", "reviewer", None, None).unwrap();
        // Should be "--- from implementer [001]" without any @scope
        assert!(lines[0].starts_with("--- from implementer [001]"));
        assert!(!lines[0].contains('@'));
    }
}
