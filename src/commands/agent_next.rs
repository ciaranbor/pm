use std::path::Path;

use crate::error::Result;
use crate::messages::{self, SenderResolution};
use crate::state::paths;

/// Advance an agent's per-sender cursor by one position. If `from` is not
/// given, the sender is resolved implicitly: exactly one sender must have
/// unread messages. Returns a single status line describing the new state.
pub fn agent_next(
    project_root: &Path,
    feature: &str,
    agent: &str,
    from: Option<&str>,
) -> Result<String> {
    let messages_dir = paths::messages_dir(project_root);

    let resolution = messages::resolve_sender(&messages_dir, feature, agent, from)?;
    if let SenderResolution::NoUnread = resolution {
        return Ok("No messages to advance past".to_string());
    }
    let sender = resolution.into_sender()?;
    let pos = messages::next(&messages_dir, feature, agent, &sender)?;
    Ok(format!("Advanced {sender} cursor to [{pos:03}]"))
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

    #[test]
    fn next_advances_implicit_sender() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "b").unwrap();

        let line = agent_next(&root, "login", "reviewer", None).unwrap();
        assert_eq!(line, "Advanced implementer cursor to [001]");

        let line = agent_next(&root, "login", "reviewer", None).unwrap();
        assert_eq!(line, "Advanced implementer cursor to [002]");
    }

    #[test]
    fn next_no_messages_friendly() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let line = agent_next(&root, "login", "reviewer", None).unwrap();
        assert_eq!(line, "No messages to advance past");
    }

    #[test]
    fn next_ambiguous_errors() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        messages::send(&mdir, "login", "reviewer", "user", "b").unwrap();

        let err = agent_next(&root, "login", "reviewer", None).unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("multiple senders"));
    }

    #[test]
    fn next_with_explicit_from() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        messages::send(&mdir, "login", "reviewer", "user", "b").unwrap();

        let line = agent_next(&root, "login", "reviewer", Some("user")).unwrap();
        assert_eq!(line, "Advanced user cursor to [001]");

        // implementer's cursor was untouched.
        assert_eq!(
            messages::cursor_for(&mdir, "login", "reviewer", "implementer").unwrap(),
            0
        );
        assert_eq!(
            messages::cursor_for(&mdir, "login", "reviewer", "user").unwrap(),
            1
        );
    }

    #[test]
    fn next_at_latest_errors() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        agent_next(&root, "login", "reviewer", None).unwrap();

        // After advancing past the only message, the inbox is empty (no unread)
        // → the friendly "No messages to advance past" path.
        let line = agent_next(&root, "login", "reviewer", None).unwrap();
        assert_eq!(line, "No messages to advance past");

        // But if the caller insists with --from, we get the hard error from
        // messages::next itself.
        let err = agent_next(&root, "login", "reviewer", Some("implementer")).unwrap_err();
        assert!(format!("{err}").contains("no messages to advance past"));
    }
}
