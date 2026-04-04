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
