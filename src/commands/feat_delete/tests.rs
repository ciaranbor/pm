//! `feat_delete` end to end: what a delete removes, what it tells `main`,
//! and when it refuses.

mod cleanup;
mod ending;
mod safety;
mod stacked;

use super::*;
use crate::messages;
use crate::testing::TestServer;
use crate::tmux;
use tempfile::tempdir;

/// `main`'s unread messages, oldest first.
fn main_inbox(project: &Path) -> Vec<String> {
    let dir = paths::messages_dir(project);
    messages::list(&dir, "main", "main", None)
        .unwrap()
        .into_iter()
        .filter(|m| m.status != messages::MessageStatus::Read)
        .map(|m| {
            messages::read_at(&dir, "main", "main", &m.sender, m.index)
                .unwrap()
                .unwrap()
                .body
        })
        .collect()
}

fn delete(server: &TestServer, project_path: &Path, force: bool) {
    feat_delete(
        project_path,
        &TestServer::registry_dir(project_path),
        "login",
        force,
        server.name(),
    )
    .unwrap();
}
