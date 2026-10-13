//! `pm msg reply`: answer the last message read from an inbox, routed to
//! the inbox its sender's waiter watches.
//!
//! Where the reply is read from (the inbox holding `.last_read`) and who it
//! is from are separate. An agent's reply (`PM_AGENT_NAME` set) routes back
//! to its home, so an agent answering from another project's inbox is still
//! answered in its own. Anyone else is routed back to the inbox it replied
//! from, as a shell sender no one can answer in turn. A message from a
//! sender with no inbox (pm itself, a shell, outside every project) has no
//! reply.

use std::path::Path;

use super::agent_send::{Home, Sender, Sent, send_as};
use crate::error::{PmError, Result};
use crate::messages;
use crate::state::paths;
use crate::state::project::ProjectEntry;
use crate::state::scope;

/// Reply as `agent` to the last message read from its inbox in
/// `inbox_scope` of the project at `inbox_root`. `home` is the replier's
/// own, if it is an agent; `projects_dir` resolves a sender in another
/// project.
pub fn msg_reply(
    projects_dir: &Path,
    inbox_root: &Path,
    inbox_scope: &str,
    agent: &str,
    home: Option<Home<'_>>,
    body: &str,
    tmux_server: Option<&str>,
) -> Result<Sent> {
    let messages_dir = paths::messages_dir(inbox_root);
    let last_read = messages::load_last_read(&messages_dir, inbox_scope, agent)?;

    let last_read = last_read.ok_or_else(|| {
        PmError::Messaging(
            "No message to reply to. Use `pm msg send --scope <scope> <agent>` for unsolicited messages."
                .to_string(),
        )
    })?;
    let nowhere = if last_read.from_outside {
        Some("from outside every project")
    } else if last_read.from_shell {
        Some("from a shell, not as an agent")
    } else if messages::is_no_reply(&last_read.sender) {
        Some("as pm itself, not as an agent")
    } else {
        None
    };
    if let Some(how) = nowhere {
        return Err(PmError::Messaging(format!(
            "'{}' sent that message {how}: there is no one to reply to. Report in your own \
             session, or as `pm workflow show` says",
            last_read.sender
        )));
    }

    let (target_root, target_scope) = match &last_read.sender_project {
        Some(project) => (
            ProjectEntry::load_here(projects_dir, project)?.1,
            last_read.sender_scope.as_deref().unwrap_or(scope::MAIN),
        ),
        None => (
            inbox_root.to_path_buf(),
            last_read.sender_scope.as_deref().unwrap_or(inbox_scope),
        ),
    };
    let replier_home = home;
    let home = home.unwrap_or(Home {
        root: inbox_root,
        scope: inbox_scope,
    });
    send_as(
        &target_root,
        target_scope,
        &last_read.sender,
        Sender {
            name: agent,
            home: Some(home),
            shell: replier_home.is_none(),
        },
        body,
        tmux_server,
    )
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
                fresh_base: None,
            },
            agents: Default::default(),
            harness: Default::default(),
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
            workflow: None,
            created: now,
            last_active: now,
            team: Default::default(),
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
            .join(".agents/agents")
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

        let err = msg_reply(&root, &root, "login", "implementer", None, "hello", None).unwrap_err();
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

        // The reply target must be an active agent — messaging no longer spawns.
        server.spawn_fake_agent(&root, &main_session, "main", "reviewer");

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
            &root,
            &feature,
            "implementer",
            Some(Home {
                root: &root,
                scope: &feature,
            }),
            "done, please review",
            server.name(),
        )
        .unwrap()
        .status;

        // Should route to main scope
        assert!(result.contains("reviewer@main"));
        assert!(result.contains("implementer@login"));

        // Verify message arrived in main scope
        let msg = messages::read_at(&messages_dir, "main", "reviewer", "implementer@login", 1)
            .unwrap()
            .unwrap();
        assert_eq!(msg.body, "done, please review");
    }

    #[test]
    fn reply_same_scope() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (root, session, feature) = setup_project_with_tmux(dir.path(), &server);

        create_agent_definition(&root, "reviewer");
        // The reply target must be an active agent — messaging no longer spawns.
        server.spawn_fake_agent(&root, &session, &feature, "reviewer");

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
        let result = msg_reply(
            &root,
            &root,
            &feature,
            "implementer",
            Some(Home {
                root: &root,
                scope: &feature,
            }),
            "got it",
            server.name(),
        )
        .unwrap()
        .status;

        // Same-scope: no @scope notation
        assert!(result.contains("Message 001 sent to 'reviewer'"));
        assert!(!result.contains("@"));
    }
}
