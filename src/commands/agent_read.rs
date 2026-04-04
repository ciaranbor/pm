use std::path::Path;

use crate::error::Result;
use crate::messages;
use crate::state::paths;

/// Read unread messages from an agent's inbox. Returns formatted message lines.
pub fn agent_read(
    project_root: &Path,
    feature: &str,
    agent: &str,
    from: Option<&str>,
) -> Result<Vec<String>> {
    let messages_dir = paths::messages_dir(project_root);
    let msgs = messages::read(&messages_dir, feature, agent, from)?;

    if msgs.is_empty() {
        return Ok(vec!["No new messages".to_string()]);
    }

    let mut lines = Vec::new();
    for msg in &msgs {
        lines.push(format!(
            "--- from {} [{:03}] {} ---",
            msg.sender,
            msg.index,
            msg.meta.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
        ));
        lines.push(msg.body.clone());
        lines.push(String::new());
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
    fn read_formats_messages_with_header() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "fix the bug").unwrap();

        let lines = agent_read(&root, "login", "reviewer", None).unwrap();
        assert!(lines[0].starts_with("--- from implementer [001]"));
        assert!(lines[0].ends_with("UTC ---"));
        assert_eq!(lines[1], "fix the bug");
    }

    #[test]
    fn read_no_messages() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let lines = agent_read(&root, "login", "reviewer", None).unwrap();
        assert_eq!(lines, vec!["No new messages"]);
    }
}
