//! A send to an agent in another project, found through the global
//! registry: it only queues, since this project can't spawn there.

use std::path::Path;

use crate::error::Result;
use crate::messages;
use crate::state::paths;
use crate::state::project::ProjectEntry;

/// Parameters for sending a message to an agent in a different project.
pub struct CrossProjectSendParams<'a> {
    pub target_project_name: &'a str,
    pub sender_scope: &'a str,
    pub sender_project: &'a str,
    pub target_scope: &'a str,
    pub recipient: &'a str,
    pub sender: &'a str,
    pub body: &'a str,
}

/// Send a message to an agent in a different project, found through the
/// global registry.
pub fn agent_send_cross_project(params: &CrossProjectSendParams<'_>) -> Result<String> {
    let projects_dir = paths::global_projects_dir()?;
    agent_send_cross_project_with_dir(&projects_dir, params)
}

/// Inner implementation that accepts an explicit `projects_dir` for testability.
fn agent_send_cross_project_with_dir(
    projects_dir: &Path,
    params: &CrossProjectSendParams<'_>,
) -> Result<String> {
    let entry = ProjectEntry::load(projects_dir, params.target_project_name)?;
    let target_root = entry.root_path();

    let messages_dir = paths::messages_dir(&target_root);
    let index = messages::send_full(
        &messages_dir,
        params.target_scope,
        params.recipient,
        params.sender,
        params.body,
        Some(params.sender_scope),
        Some(params.sender_project),
    )?;

    Ok(format!(
        "Message {index:03} sent to '{recipient}@{target_scope}' in project '{target_project_name}' \
         (from '{sender}@{sender_scope}' in project '{sender_project}')",
        recipient = params.recipient,
        target_scope = params.target_scope,
        target_project_name = params.target_project_name,
        sender = params.sender,
        sender_scope = params.sender_scope,
        sender_project = params.sender_project,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PmError;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Helper: set up a minimal project root with .pm dir.
    fn setup_target_project(dir: &Path) -> PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/messages")).unwrap();
        root
    }

    #[test]
    fn cross_project_send_delivers_message() {
        let target_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();

        let target_root = setup_target_project(target_dir.path());

        // Register target project in the projects directory
        let entry = ProjectEntry {
            root: target_root.to_str().unwrap().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(projects_dir.path(), "exo").unwrap();

        let result = agent_send_cross_project_with_dir(
            projects_dir.path(),
            &CrossProjectSendParams {
                target_project_name: "exo",
                sender_scope: "login",
                sender_project: "myapp",
                target_scope: "main",
                recipient: "implementer",
                sender: "reviewer",
                body: "found a bug in the auth module",
            },
        )
        .unwrap();

        assert!(result.contains("Message 001"));
        assert!(result.contains("implementer@main"));
        assert!(result.contains("project 'exo'"));
        assert!(result.contains("reviewer@login"));
        assert!(result.contains("project 'myapp'"));

        // Verify message was actually delivered to the target project
        let messages_dir = paths::messages_dir(&target_root);
        let msg = messages::read_at(&messages_dir, "main", "implementer", "reviewer", 1)
            .unwrap()
            .unwrap();
        assert_eq!(msg.body, "found a bug in the auth module");
        assert_eq!(msg.meta.sender_scope.as_deref(), Some("login"));
        assert_eq!(msg.meta.sender_project.as_deref(), Some("myapp"));
    }

    #[test]
    fn cross_project_send_to_nonexistent_project_errors() {
        let projects_dir = tempdir().unwrap();
        std::fs::create_dir_all(projects_dir.path()).unwrap();

        let result = agent_send_cross_project_with_dir(
            projects_dir.path(),
            &CrossProjectSendParams {
                target_project_name: "nonexistent",
                sender_scope: "login",
                sender_project: "myapp",
                target_scope: "main",
                recipient: "implementer",
                sender: "reviewer",
                body: "hello",
            },
        );
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::ProjectNotFound(_)));
    }

    #[test]
    fn cross_project_send_records_sender_metadata() {
        let target_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();

        let target_root = setup_target_project(target_dir.path());

        let entry = ProjectEntry {
            root: target_root.to_str().unwrap().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(projects_dir.path(), "exo").unwrap();

        agent_send_cross_project_with_dir(
            projects_dir.path(),
            &CrossProjectSendParams {
                target_project_name: "exo",
                sender_scope: "my-feature",
                sender_project: "myapp",
                target_scope: "main",
                recipient: "bot",
                sender: "human",
                body: "test message",
            },
        )
        .unwrap();

        let messages_dir = paths::messages_dir(&target_root);
        let msg = messages::read_at(&messages_dir, "main", "bot", "human", 1)
            .unwrap()
            .unwrap();
        assert_eq!(msg.meta.sender_scope.as_deref(), Some("my-feature"));
        assert_eq!(msg.meta.sender_project.as_deref(), Some("myapp"));
    }

    #[test]
    fn cross_project_send_increments_index() {
        let target_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();

        let target_root = setup_target_project(target_dir.path());

        let entry = ProjectEntry {
            root: target_root.to_str().unwrap().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(projects_dir.path(), "exo").unwrap();

        let r1 = agent_send_cross_project_with_dir(
            projects_dir.path(),
            &CrossProjectSendParams {
                target_project_name: "exo",
                sender_scope: "feat",
                sender_project: "myapp",
                target_scope: "main",
                recipient: "bot",
                sender: "human",
                body: "first",
            },
        )
        .unwrap();
        assert!(r1.contains("Message 001"));

        let r2 = agent_send_cross_project_with_dir(
            projects_dir.path(),
            &CrossProjectSendParams {
                target_project_name: "exo",
                sender_scope: "feat",
                sender_project: "myapp",
                target_scope: "main",
                recipient: "bot",
                sender: "human",
                body: "second",
            },
        )
        .unwrap();
        assert!(r2.contains("Message 002"));
    }
}
