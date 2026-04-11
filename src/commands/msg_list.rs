use std::path::Path;

use crate::error::Result;
use crate::messages::{self, MessageStatus};
use crate::state::paths;

/// List all messages in an agent's inbox (optionally scoped to one sender),
/// formatted for printing. Grouped by sender, with status markers showing
/// which message is next-to-read.
pub fn msg_list(
    project_root: &Path,
    feature: &str,
    agent: &str,
    from: Option<&str>,
) -> Result<Vec<String>> {
    let messages_dir = paths::messages_dir(project_root);
    let summaries = messages::list(&messages_dir, feature, agent, from)?;

    if summaries.is_empty() {
        return Ok(vec!["No messages".to_string()]);
    }

    let mut lines = Vec::new();
    let mut current_sender: Option<String> = None;
    for s in &summaries {
        if current_sender.as_deref() != Some(s.sender.as_str()) {
            if current_sender.is_some() {
                lines.push(String::new());
            }
            lines.push(format!("from {}:", s.sender));
            current_sender = Some(s.sender.clone());
        }

        let marker = match s.status {
            MessageStatus::Read => "  ",
            MessageStatus::Next => "> ",
            MessageStatus::Queued => "  ",
        };
        let status_tag = match s.status {
            MessageStatus::Read => "read  ",
            MessageStatus::Next => "next  ",
            MessageStatus::Queued => "queued",
        };

        lines.push(format!(
            "  {marker}[{:03}] {} {}  {}",
            s.index,
            s.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
            status_tag,
            s.first_line
        ));
    }

    Ok(lines)
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
    fn list_empty_inbox() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let lines = msg_list(&root, "login", "reviewer", None).unwrap();
        assert_eq!(lines, vec!["No messages"]);
    }

    #[test]
    fn list_groups_by_sender_with_status_markers() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "two").unwrap();
        messages::send(&mdir, "login", "reviewer", "user", "hello from user").unwrap();
        messages::next(&mdir, "login", "reviewer", "implementer").unwrap();

        let lines = msg_list(&root, "login", "reviewer", None).unwrap();
        // Expect: "from implementer:", then 001 (read), 002 (next),
        // blank, "from user:", then 001 (next).
        assert_eq!(lines[0], "from implementer:");
        assert!(lines[1].contains("[001]"));
        assert!(lines[1].contains("read"));
        assert!(lines[1].contains("one"));
        assert!(lines[2].contains("[002]"));
        assert!(lines[2].contains("next"));
        assert!(lines[2].starts_with("  > "));
        assert!(lines[2].contains("two"));
        assert_eq!(lines[3], "");
        assert_eq!(lines[4], "from user:");
        assert!(lines[5].contains("[001]"));
        assert!(lines[5].contains("next"));
        assert!(lines[5].contains("hello from user"));
    }

    #[test]
    fn list_scoped_to_one_sender() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "impl").unwrap();
        messages::send(&mdir, "login", "reviewer", "user", "user").unwrap();

        let lines = msg_list(&root, "login", "reviewer", Some("user")).unwrap();
        assert_eq!(lines[0], "from user:");
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("user"));
    }

    #[test]
    fn list_all_queued_when_cursor_past_end_is_impossible() {
        // After advancing past everything, the whole inbox should show as "read".
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "a").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "b").unwrap();
        messages::next(&mdir, "login", "reviewer", "implementer").unwrap();
        messages::next(&mdir, "login", "reviewer", "implementer").unwrap();

        let lines = msg_list(&root, "login", "reviewer", None).unwrap();
        assert!(lines[1].contains("read"));
        assert!(lines[2].contains("read"));
    }
}
