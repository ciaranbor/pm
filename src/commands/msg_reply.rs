use std::path::Path;

use crate::error::{PmError, Result};
use crate::messages;
use crate::state::paths;

/// Reply to the last-read message, auto-routing to the sender's scope.
///
/// Loads `.last_read` from the agent's inbox, determines the target scope
/// from the original message metadata, and sends the reply via the existing
/// `agent_send` / `agent_send_cross_project` machinery.
///
/// Returns status lines suitable for printing.
pub fn msg_reply(
    project_root: &Path,
    current_scope: &str,
    agent: &str,
    body: &str,
    tmux_server: Option<&str>,
) -> Result<String> {
    let messages_dir = paths::messages_dir(project_root);
    let last_read = messages::load_last_read(&messages_dir, current_scope, agent)?;

    let last_read = last_read.ok_or_else(|| {
        PmError::Messaging(
            "No message to reply to. Use `pm msg send --scope <scope> <agent>` for unsolicited messages."
                .to_string(),
        )
    })?;

    if let Some(ref sender_project) = last_read.sender_project {
        // Cross-project reply
        let pm_dir = paths::pm_dir(project_root);
        let config = crate::state::project::ProjectConfig::load(&pm_dir)?;
        let our_project = &config.project.name;
        let target_scope = last_read.sender_scope.as_deref().unwrap_or("main");

        super::agent_send::agent_send_cross_project(&super::agent_send::CrossProjectSendParams {
            target_project_name: sender_project,
            sender_scope: current_scope,
            sender_project: our_project,
            target_scope,
            recipient: &last_read.sender,
            sender: agent,
            body,
        })
    } else {
        // Same-project reply: use sender_scope as target if set
        let target_scope = last_read.sender_scope.as_deref();
        super::agent_send::agent_send(
            project_root,
            current_scope,
            target_scope,
            &last_read.sender,
            agent,
            body,
            tmux_server,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages;
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::state::project::{ProjectConfig, ProjectInfo};
    use crate::testing::TestServer;
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn setup_project_with_tmux(dir: &Path, server: &TestServer) -> (PathBuf, String, String) {
        let root = dir.to_path_buf();
        let pm_dir = root.join(".pm");
        let project_name = server.scope("proj");
        let feature_name = "login";

        std::fs::create_dir_all(pm_dir.join("features")).unwrap();

        let config = ProjectConfig {
            project: ProjectInfo {
                name: project_name.clone(),
                max_features: None,
            },
            setup: Default::default(),
            github: Default::default(),
            agents: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        let now = Utc::now();
        let state = FeatureState {
            status: FeatureStatus::Wip,
            branch: feature_name.to_string(),
            worktree: feature_name.to_string(),
            base: "main".to_string(),
            pr: String::new(),
            context: String::new(),
            created: now,
            last_active: now,
        };
        state.save(&pm_dir.join("features"), feature_name).unwrap();

        let worktree = root.join(feature_name);
        std::fs::create_dir_all(&worktree).unwrap();

        let session_name = crate::tmux::session_name(&project_name, feature_name);
        crate::tmux::create_session(server.name(), &session_name, &worktree).unwrap();

        (root, session_name, feature_name.to_string())
    }

    fn create_agent_definition(root: &Path, agent_name: &str) {
        let agent_def = paths::main_worktree(root)
            .join(".claude/agents")
            .join(format!("{agent_name}.md"));
        std::fs::create_dir_all(agent_def.parent().unwrap()).unwrap();
        std::fs::write(&agent_def, "# agent stub").unwrap();
    }

    fn setup_project_no_tmux(dir: &Path) -> PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        root
    }

    #[test]
    fn reply_no_last_read_errors() {
        let dir = tempdir().unwrap();
        let root = setup_project_no_tmux(dir.path());

        let err = msg_reply(&root, "login", "implementer", "hello", None).unwrap_err();
        assert!(format!("{err}").contains("No message to reply to"));
    }

    #[test]
    fn reply_routes_to_sender_scope() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session, feature) = setup_project_with_tmux(dir.path(), &server);

        // Create agent definition for the reply target
        create_agent_definition(&root, "reviewer");

        // Set up main scope with tmux session
        let pm_dir = root.join(".pm");
        let config = ProjectConfig::load(&pm_dir).unwrap();
        let main_worktree = paths::main_worktree(&root);
        std::fs::create_dir_all(&main_worktree).unwrap();
        let main_session = crate::tmux::session_name(&config.project.name, "main");
        crate::tmux::create_session(server.name(), &main_session, &main_worktree).unwrap();

        let now = Utc::now();
        let main_state = FeatureState {
            status: FeatureStatus::Wip,
            branch: "main".to_string(),
            worktree: "main".to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            created: now,
            last_active: now,
        };
        main_state.save(&pm_dir.join("features"), "main").unwrap();

        // Send a cross-scope message from main→login
        let messages_dir = paths::messages_dir(&root);
        messages::send_with_scope(
            &messages_dir,
            &feature,
            "implementer",
            "reviewer",
            "please implement this",
            Some("main"),
        )
        .unwrap();

        // Read it (writes .last_read)
        crate::commands::agent_read::agent_read(&root, &feature, "implementer", None, None)
            .unwrap();

        // Reply
        let result = msg_reply(
            &root,
            &feature,
            "implementer",
            "done, please review",
            server.name(),
        )
        .unwrap();

        // Should route to main scope
        assert!(result.contains("reviewer@main"));
        assert!(result.contains("implementer@login"));

        // Verify message arrived in main scope
        let msg = messages::read_at(&messages_dir, "main", "reviewer", "implementer", 1)
            .unwrap()
            .unwrap();
        assert_eq!(msg.body, "done, please review");
    }

    #[test]
    fn reply_same_scope() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, _session, feature) = setup_project_with_tmux(dir.path(), &server);

        create_agent_definition(&root, "reviewer");

        // Send same-scope message (no sender_scope)
        let messages_dir = paths::messages_dir(&root);
        messages::send(
            &messages_dir,
            &feature,
            "implementer",
            "reviewer",
            "same scope msg",
        )
        .unwrap();

        // Read it
        crate::commands::agent_read::agent_read(&root, &feature, "implementer", None, None)
            .unwrap();

        // Reply — should stay same-scope
        let result = msg_reply(&root, &feature, "implementer", "got it", server.name()).unwrap();

        // Same-scope: no @scope notation
        assert!(result.contains("Message 001 sent to 'reviewer'"));
        assert!(!result.contains("@"));
    }

    #[test]
    fn reply_cross_project() {
        let dir = tempdir().unwrap();
        let root = setup_project_no_tmux(dir.path());

        // Set up project config so we have a project name
        let pm_dir = root.join(".pm");
        let config = ProjectConfig {
            project: ProjectInfo {
                name: "myapp".to_string(),
                max_features: None,
            },
            setup: Default::default(),
            github: Default::default(),
            agents: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        // Set up a "source project" in a separate temp dir
        let source_dir = tempdir().unwrap();
        let source_root = source_dir.path().to_path_buf();
        std::fs::create_dir_all(source_root.join(".pm/messages")).unwrap();

        // Register the source project in global projects dir
        let projects_dir = tempdir().unwrap();
        let entry = crate::state::project::ProjectEntry {
            root: source_root.to_str().unwrap().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(projects_dir.path(), "source-proj").unwrap();

        // Simulate receiving a cross-project message
        let messages_dir = paths::messages_dir(&root);
        messages::send_full(
            &messages_dir,
            "login",
            "implementer",
            "reviewer",
            "cross-project request",
            Some("main"),
            Some("source-proj"),
        )
        .unwrap();

        // Read it (writes .last_read with sender_project)
        crate::commands::agent_read::agent_read(&root, "login", "implementer", None, None).unwrap();

        // Verify .last_read has cross-project info
        let lr = messages::load_last_read(&messages_dir, "login", "implementer")
            .unwrap()
            .unwrap();
        assert_eq!(lr.sender, "reviewer");
        assert_eq!(lr.sender_scope.as_deref(), Some("main"));
        assert_eq!(lr.sender_project.as_deref(), Some("source-proj"));
        assert_eq!(lr.index, 1);
    }
}
