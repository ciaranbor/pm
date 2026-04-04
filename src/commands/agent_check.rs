use std::path::Path;

use crate::error::Result;
use crate::messages;
use crate::state::paths;

/// Check for unread messages in an agent's inbox. Returns status lines.
pub fn agent_check(project_root: &Path, feature: &str, agent: &str) -> Result<Vec<String>> {
    let messages_dir = paths::messages_dir(project_root);
    let summaries = messages::check(&messages_dir, feature, agent)?;

    if summaries.is_empty() {
        return Ok(vec!["No new messages".to_string()]);
    }

    let total: u32 = summaries.iter().map(|s| s.count).sum();
    let mut lines = vec![format!(
        "{total} new message{}",
        if total == 1 { "" } else { "s" }
    )];

    for summary in &summaries {
        lines.push(format!("  {} from {}", summary.count, summary.sender));
    }

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn setup_project(dir: &Path) -> PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        // Create a fake feature state file so resolve_feature works
        std::fs::write(root.join(".pm/features/login.toml"), "").unwrap();
        root
    }

    #[test]
    fn singular_message() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "hi").unwrap();

        let lines = agent_check(&root, "login", "reviewer").unwrap();
        assert_eq!(lines[0], "1 new message");
        assert_eq!(lines[1], "  1 from implementer");
    }

    #[test]
    fn plural_messages() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let mdir = paths::messages_dir(&root);
        messages::send(&mdir, "login", "reviewer", "implementer", "one").unwrap();
        messages::send(&mdir, "login", "reviewer", "implementer", "two").unwrap();
        messages::send(&mdir, "login", "reviewer", "user", "three").unwrap();

        let lines = agent_check(&root, "login", "reviewer").unwrap();
        assert_eq!(lines[0], "3 new messages");
    }

    #[test]
    fn no_messages() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let lines = agent_check(&root, "login", "reviewer").unwrap();
        assert_eq!(lines, vec!["No new messages"]);
    }
}
