//! `pm feat status`: the team's own account of where a feature stands.
//!
//! `ready` is the orchestrator's single triage trigger: it requires a
//! non-empty summary and messages `main` each time it is set, so re-marking
//! a feature ready is how a revised summary reaches `main`. The message
//! carries the sender's feature scope, so `main` can reply to the agent with
//! follow-up questions while the team is still running. `blocked` and `wip`
//! message no one.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::messages;
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;

/// Set a feature's progress. `agent` is the agent running the command, if
/// any: `main` can reply to it about a `ready` message.
pub fn feat_status(
    project_root: &Path,
    name: &str,
    progress: Progress,
    agent: Option<&str>,
) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    let mut state = FeatureState::load(&features_dir, name)?;

    let summary = paths::summary_path(project_root, name);
    let written = std::fs::read_to_string(&summary).is_ok_and(|s| !s.trim().is_empty());
    if progress == Progress::Ready && !written {
        let mut msg = format!(
            "feature '{name}' has no summary; write one at {} (`pm feat summary path`) \
             before marking it ready",
            summary.display()
        );
        let legacy = project_root.join(&state.worktree).join("summary.md");
        if legacy.exists() {
            msg.push_str(&format!(
                ". Its worktree has a summary.md: move it there with `mv {} {}`",
                legacy.display(),
                summary.display()
            ));
        }
        return Err(PmError::Summary(msg));
    }

    state.progress = progress;
    state.last_active = chrono::Utc::now();
    state.save(&features_dir, name)?;

    if progress == Progress::Ready {
        notify_ready(project_root, name, agent)?;
    }
    Ok(())
}

/// Tell `main` that `name` is ready and where its summary is. The message
/// always carries the feature's scope, which is how cleanup recognises it
/// as still unread.
fn notify_ready(project_root: &Path, name: &str, agent: Option<&str>) -> Result<()> {
    let body = ready_body(name, agent.is_some());
    let sender = agent.map_or_else(messages::default_user_name, str::to_string);
    messages::send_with_scope(
        &paths::messages_dir(project_root),
        "main",
        "main",
        &sender,
        &body,
        Some(name),
    )?;
    Ok(())
}

/// The ready message for `name`; `repliable` when an agent sent it.
pub(crate) fn ready_body(name: &str, repliable: bool) -> String {
    let mut body = format!(
        "Feature '{name}' is ready: its work is done and waits on the user to merge or delete it. \
         Triage its summary at .pm/summaries/{name}.md."
    );
    if repliable {
        body.push_str(" The team is still running; reply here with any follow-up questions.");
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn main_inbox(project: &Path) -> Vec<messages::UnreadSummary> {
        messages::check(&paths::messages_dir(project), "main", "main").unwrap()
    }

    fn progress(project: &Path, name: &str) -> Progress {
        FeatureState::load(&paths::features_dir(project), name)
            .unwrap()
            .progress
    }

    #[test]
    fn ready_without_summary_is_refused_and_tells_no_one() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");

        let err = feat_status(&project, "login", Progress::Ready, Some("implementer")).unwrap_err();

        assert!(err.to_string().contains("pm feat summary path"), "{err}");
        assert!(!err.to_string().contains("summary.md"), "{err}");
        assert_eq!(progress(&project, "login"), Progress::Wip);
        assert!(main_inbox(&project).is_empty());
    }

    #[test]
    fn ready_refusal_points_at_a_legacy_worktree_summary() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        std::fs::write(project.join("login/summary.md"), "notes").unwrap();

        let err = feat_status(&project, "login", Progress::Ready, Some("implementer")).unwrap_err();

        let hint = format!(
            "`mv {} {}`",
            project.join("login/summary.md").display(),
            paths::summary_path(&project, "login").display()
        );
        assert!(err.to_string().contains(&hint), "{err}");
    }

    #[test]
    fn ready_with_summary_notifies_main_from_the_agent_in_its_scope() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        std::fs::write(
            crate::commands::feat_summary::path(&project, "login").unwrap(),
            "notes",
        )
        .unwrap();

        feat_status(&project, "login", Progress::Ready, Some("implementer")).unwrap();

        assert_eq!(progress(&project, "login"), Progress::Ready);
        let inbox = main_inbox(&project);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].sender, "implementer");
        assert_eq!(inbox[0].count, 1);
        let msg = messages::read_at(
            &paths::messages_dir(&project),
            "main",
            "main",
            "implementer",
            1,
        )
        .unwrap()
        .unwrap();
        assert_eq!(msg.meta.sender_scope.as_deref(), Some("login"));
        assert!(msg.body.contains(".pm/summaries/login.md"), "{}", msg.body);
    }

    #[test]
    fn ready_with_an_empty_summary_is_refused() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        std::fs::write(
            crate::commands::feat_summary::path(&project, "login").unwrap(),
            " \n",
        )
        .unwrap();

        assert!(feat_status(&project, "login", Progress::Ready, Some("implementer")).is_err());
        assert!(main_inbox(&project).is_empty());
    }

    #[test]
    fn blocked_and_wip_tell_no_one() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");

        feat_status(&project, "login", Progress::Blocked, Some("implementer")).unwrap();
        assert_eq!(progress(&project, "login"), Progress::Blocked);
        feat_status(&project, "login", Progress::Wip, Some("implementer")).unwrap();
        assert_eq!(progress(&project, "login"), Progress::Wip);

        assert!(main_inbox(&project).is_empty());
    }
}
