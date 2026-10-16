//! `pm feat status`: the team's own account of where a scope stands.
//!
//! Every scope has one ([`TeamStatus`]). `ready` is a feature's alone: it
//! means waiting on a merge or delete, which main never is. It requires a
//! non-empty summary and tells `main` ([`ready_notice`] has when, and when
//! not again). `main` reviews it for gaps; triage waits for the merge or
//! delete notice (see [`super::feat_delete`]). The notice carries the
//! sender's feature scope, so `main` can reply to the agent with follow-up
//! questions while the team is still running. `blocked` and `wip` message
//! no one.
//!
//! A blocked scope may carry a reason, the question the user is to answer,
//! and records the agent that set it. Any later status change drops both.

use std::path::Path;

use super::ready_notice::{self, Notice};
use crate::error::{PmError, Result};
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;
use crate::state::scope::{self, TeamStatus};

/// Set `scope`'s progress, with `reason` only for `blocked`. `agent` is the
/// agent running the command, if any: recorded as the one a `blocked` scope
/// waits in, and the one `main` can reply to about a `ready` message.
/// Returns what became of the ready notice when marking a feature ready.
pub fn feat_status(
    project_root: &Path,
    scope: &str,
    progress: Progress,
    reason: Option<&str>,
    agent: Option<&str>,
    tmux_server: Option<&str>,
) -> Result<Option<Notice>> {
    if reason.is_some() && progress != Progress::Blocked {
        return Err(PmError::SafetyCheck(format!(
            "a reason is only given with `blocked`, not `{progress}`"
        )));
    }
    if progress == Progress::Ready {
        check_ready(project_root, scope)?;
    }
    TeamStatus {
        progress,
        blocked_reason: reason
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string),
        blocked_by: agent
            .filter(|_| progress == Progress::Blocked)
            .map(str::to_string),
    }
    .save(project_root, scope)?;

    if progress == Progress::Ready {
        return Ok(Some(ready_notice::mark_ready(
            project_root,
            scope,
            agent,
            tmux_server,
        )?));
    }
    if scope != scope::MAIN {
        ready_notice::leave_ready(project_root, scope)?;
    }
    Ok(None)
}

/// `Ok` if `scope` may be marked ready: a feature with a summary.
fn check_ready(project_root: &Path, scope: &str) -> Result<()> {
    if scope == scope::MAIN {
        return Err(PmError::SafetyCheck(
            "main can't be ready: ready means waiting on a merge or delete, and main has neither"
                .into(),
        ));
    }
    FeatureState::load(&paths::features_dir(project_root), scope)?;
    let summary = paths::summary_path(project_root, scope);
    if std::fs::read_to_string(&summary).is_ok_and(|s| !s.trim().is_empty()) {
        return Ok(());
    }
    Err(PmError::Summary(format!(
        "feature '{scope}' has no summary; write one at {} (`pm feat summary path`) \
         before marking it ready",
        summary.display()
    )))
}

/// What `pm feat status [STATUS] [NAME] [-m REASON]` asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Set {
        progress: Progress,
        name: Option<String>,
    },
    /// The attention view, of `name` or of the scope run from.
    View { name: Option<String> },
}

/// Read the positionals: a first word that is not a status is the name of
/// a scope to view.
pub fn request(
    project_root: &Path,
    status: Option<String>,
    name: Option<String>,
    has_reason: bool,
) -> Result<Request> {
    let request = match (status, name) {
        (None, name) => Request::View { name },
        (Some(word), name) => match <Progress as clap::ValueEnum>::from_str(&word, false) {
            Ok(progress) => Request::Set { progress, name },
            Err(_) if name.is_none() => match scope::check(project_root, &word) {
                Ok(()) => Request::View { name: Some(word) },
                Err(e @ PmError::WorktreeMissing { .. }) => return Err(e),
                Err(_) => {
                    return Err(PmError::SafetyCheck(format!(
                        "'{word}' is neither a status (wip, blocked, ready) nor a scope"
                    )));
                }
            },
            Err(_) => {
                return Err(PmError::SafetyCheck(format!(
                    "'{word}' is not a status (wip, blocked, ready)"
                )));
            }
        },
    };
    if has_reason && matches!(request, Request::View { .. }) {
        return Err(PmError::SafetyCheck(
            "--reason needs a status: `pm feat status blocked -m …`".into(),
        ));
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn main_inbox(project: &Path) -> Vec<messages::UnreadSummary> {
        messages::check(&paths::messages_dir(project), "main", "main").unwrap()
    }

    fn progress(project: &Path, name: &str) -> Progress {
        TeamStatus::load(project, name).unwrap().progress
    }

    #[test]
    fn ready_without_summary_is_refused_and_tells_no_one() {
        let dir = tempdir().unwrap();
        let (project, _, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");

        let err = feat_status(
            &project,
            "login",
            Progress::Ready,
            None,
            Some("implementer"),
            crate::testing::NO_SERVER,
        )
        .unwrap_err();

        assert!(err.to_string().contains("pm feat summary path"), "{err}");
        assert_eq!(progress(&project, "login"), Progress::Wip);
        assert!(main_inbox(&project).is_empty());
    }

    #[test]
    fn ready_with_summary_notifies_main_from_the_agent_in_its_scope() {
        let dir = tempdir().unwrap();
        let (project, _, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        std::fs::write(
            crate::commands::feat_summary::path(&project, "login").unwrap(),
            "notes",
        )
        .unwrap();

        feat_status(
            &project,
            "login",
            Progress::Ready,
            None,
            Some("implementer"),
            crate::testing::NO_SERVER,
        )
        .unwrap();

        assert_eq!(progress(&project, "login"), Progress::Ready);
        let inbox = main_inbox(&project);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].sender, "implementer@login");
        assert_eq!(inbox[0].count, 1);
        let msg = messages::read_at(
            &paths::messages_dir(&project),
            "main",
            "main",
            "implementer@login",
            1,
        )
        .unwrap()
        .unwrap();
        assert_eq!(msg.meta.sender_scope.as_deref(), Some("login"));
        assert!(
            msg.body
                .contains(&paths::summary_path(&project, "login").display().to_string()),
            "{}",
            msg.body
        );
    }

    #[test]
    fn ready_with_an_empty_summary_is_refused() {
        let dir = tempdir().unwrap();
        let (project, _, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        std::fs::write(
            crate::commands::feat_summary::path(&project, "login").unwrap(),
            " \n",
        )
        .unwrap();

        assert!(
            feat_status(
                &project,
                "login",
                Progress::Ready,
                None,
                Some("implementer"),
                crate::testing::NO_SERVER
            )
            .is_err()
        );
        assert!(main_inbox(&project).is_empty());
    }

    #[test]
    fn blocked_and_wip_tell_no_one() {
        let dir = tempdir().unwrap();
        let (project, _, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");

        feat_status(
            &project,
            "login",
            Progress::Blocked,
            None,
            Some("implementer"),
            crate::testing::NO_SERVER,
        )
        .unwrap();
        assert_eq!(progress(&project, "login"), Progress::Blocked);
        feat_status(
            &project,
            "login",
            Progress::Wip,
            None,
            Some("implementer"),
            crate::testing::NO_SERVER,
        )
        .unwrap();
        assert_eq!(progress(&project, "login"), Progress::Wip);

        assert!(main_inbox(&project).is_empty());
    }

    #[test]
    fn a_blocked_reason_and_its_agent_are_kept_until_the_status_changes() {
        let dir = tempdir().unwrap();
        let (project, _, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        let blocked = |project: &Path| {
            let team = TeamStatus::load(project, "login").unwrap();
            (team.blocked_reason, team.blocked_by)
        };

        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            Some("implementer"),
            crate::testing::NO_SERVER,
        )
        .unwrap();
        assert_eq!(
            blocked(&project),
            (Some("which DB?".into()), Some("implementer".into()))
        );

        feat_status(
            &project,
            "login",
            Progress::Wip,
            None,
            Some("implementer"),
            crate::testing::NO_SERVER,
        )
        .unwrap();
        assert_eq!(blocked(&project), (None, None));
    }

    #[test]
    fn main_is_blocked_or_wip_never_ready_and_tells_no_one() {
        let dir = tempdir().unwrap();
        let (project, _, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");

        feat_status(
            &project,
            "main",
            Progress::Blocked,
            Some("next item?"),
            Some("main"),
            crate::testing::NO_SERVER,
        )
        .unwrap();
        let blocked = TeamStatus::load(&project, "main").unwrap();
        assert_eq!(
            (blocked.reason(), blocked.agent()),
            (Some("next item?"), Some("main"))
        );

        let err = feat_status(
            &project,
            "main",
            Progress::Ready,
            None,
            Some("main"),
            crate::testing::NO_SERVER,
        )
        .unwrap_err();
        assert!(err.to_string().contains("main can't be ready"), "{err}");
        assert_eq!(TeamStatus::load(&project, "main").unwrap(), blocked);

        feat_status(
            &project,
            "main",
            Progress::Wip,
            None,
            Some("main"),
            crate::testing::NO_SERVER,
        )
        .unwrap();
        assert_eq!(progress(&project, "main"), Progress::Wip);
        assert_eq!(progress(&project, "login"), Progress::Wip);
        assert!(main_inbox(&project).is_empty());
    }

    #[test]
    fn a_reason_with_any_status_but_blocked_is_refused() {
        let dir = tempdir().unwrap();
        let (project, _, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        std::fs::write(
            crate::commands::feat_summary::path(&project, "login").unwrap(),
            "notes",
        )
        .unwrap();

        for status in [Progress::Wip, Progress::Ready] {
            let err = feat_status(
                &project,
                "login",
                status,
                Some("why"),
                None,
                crate::testing::NO_SERVER,
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("only given with `blocked`"),
                "{err}"
            );
        }
        assert_eq!(progress(&project, "login"), Progress::Wip);
        assert!(main_inbox(&project).is_empty());
    }

    #[test]
    fn request_reads_a_word_as_a_status_else_as_a_feature() {
        let dir = tempdir().unwrap();
        let (project, _, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        let req = |status: Option<&str>, name: Option<&str>, reason| {
            request(
                &project,
                status.map(str::to_string),
                name.map(str::to_string),
                reason,
            )
        };
        let view = |name: Option<&str>| Request::View {
            name: name.map(str::to_string),
        };

        assert_eq!(req(None, None, false).unwrap(), view(None));
        assert_eq!(
            req(Some("blocked"), None, true).unwrap(),
            Request::Set {
                progress: Progress::Blocked,
                name: None
            }
        );
        assert_eq!(
            req(Some("ready"), Some("login"), false).unwrap(),
            Request::Set {
                progress: Progress::Ready,
                name: Some("login".to_string())
            }
        );
        assert_eq!(
            req(Some("login"), None, false).unwrap(),
            view(Some("login"))
        );
        assert_eq!(req(Some("main"), None, false).unwrap(), view(Some("main")));
        assert_eq!(
            req(Some("blocked"), Some("main"), false).unwrap(),
            Request::Set {
                progress: Progress::Blocked,
                name: Some("main".to_string())
            }
        );

        let err = |status, name, reason| req(status, name, reason).unwrap_err().to_string();
        assert!(err(Some("bogus"), Some("login"), false).contains("'bogus' is not a status"));
        assert!(
            err(Some("blcked"), None, false)
                .contains("'blcked' is neither a status (wip, blocked, ready) nor a scope")
        );
        assert!(err(None, None, true).contains("--reason needs a status"));
        assert!(err(Some("login"), None, true).contains("--reason needs a status"));

        std::fs::remove_dir_all(paths::main_worktree(&project)).unwrap();
        assert!(
            err(Some("main"), None, false).starts_with("main worktree missing at "),
            "{}",
            err(Some("main"), None, false)
        );
    }
}
