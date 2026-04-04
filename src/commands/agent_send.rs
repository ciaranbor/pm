use std::path::Path;

use crate::error::Result;
use crate::messages;
use crate::state::paths;

/// Send a message to an agent's inbox. Returns a status line.
pub fn agent_send(
    project_root: &Path,
    feature: &str,
    recipient: &str,
    sender: &str,
    body: &str,
) -> Result<String> {
    let messages_dir = paths::messages_dir(project_root);
    let index = messages::send(&messages_dir, feature, recipient, sender, body)?;
    Ok(format!(
        "Message {index:03} sent to '{recipient}' (from '{sender}')"
    ))
}
