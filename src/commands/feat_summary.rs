//! `pm feat summary`: the feature's summary for the orchestrator.
//!
//! It lives in pm state (`.pm/summaries/<feature>.md`), not in the worktree,
//! so it never lands on the branch and outlives it. Agents edit it with
//! their own file tools at the path `pm feat summary path` prints; pm never
//! sees an edit, so a change to a ready feature's summary reaches `main`
//! only by marking it ready again (see [`super::feat_status`]).
//!
//! A merge or delete keeps the summary for `main`, which triages it on the
//! cleanup notice and deletes it; until then the name cannot be reused.
//! A `summary.md` left in the worktree by features that predate this is
//! collected there too.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::state::feature::FeatureState;
use crate::state::paths;

/// Where a feature's summary is edited. Its directory exists; the file
/// may not yet.
pub fn path(project_root: &Path, name: &str) -> Result<PathBuf> {
    FeatureState::load(&paths::features_dir(project_root), name)?;
    std::fs::create_dir_all(paths::summaries_dir(project_root))?;
    Ok(paths::summary_path(project_root, name))
}

/// A feature's summary.
pub fn show(project_root: &Path, name: &str) -> Result<String> {
    let path = paths::summary_path(project_root, name);
    if !path.exists() {
        return Err(PmError::Summary(format!("no summary for feature '{name}'")));
    }
    Ok(std::fs::read_to_string(path)?)
}

/// Refuse to reuse `name` while an earlier feature's summary under that
/// name waits for `main`: the new feature would take it over.
pub(crate) fn ensure_no_untriaged(project_root: &Path, name: &str) -> Result<()> {
    let summary = paths::summary_path(project_root, name);
    if summary.exists() {
        return Err(PmError::Summary(format!(
            "a summary from an earlier feature '{name}' at {} is waiting for main to triage; \
             triage then delete it, or pick another name",
            summary.display()
        )));
    }
    Ok(())
}

/// Keep a feature's summary for `main` before its worktree goes. Returns
/// its path, if there is one.
pub(crate) fn collect(project_root: &Path, worktree: &Path, name: &str) -> Result<Option<PathBuf>> {
    let summary = paths::summary_path(project_root, name);
    let legacy = worktree.join("summary.md");
    if legacy.exists() {
        if summary.exists() {
            eprintln!(
                "warning: dropped {} in favour of the summary in pm state",
                legacy.display()
            );
        } else {
            // Copied, not moved: a committed summary.md would otherwise leave
            // the worktree dirty and block its removal.
            std::fs::create_dir_all(paths::summaries_dir(project_root))?;
            std::fs::copy(&legacy, &summary)?;
        }
    }
    Ok(summary.exists().then_some(summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn write(project: &Path, name: &str, body: &str) {
        std::fs::write(path(project, name).unwrap(), body).unwrap();
    }

    #[test]
    fn path_is_where_show_reads_and_is_outside_the_worktree() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");

        assert!(show(&project, "login").is_err());
        write(&project, "login", "notes");

        assert_eq!(show(&project, "login").unwrap(), "notes");
        assert!(
            !path(&project, "login")
                .unwrap()
                .starts_with(project.join("login"))
        );
    }

    #[test]
    fn path_of_an_unknown_feature_is_refused() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");

        assert!(matches!(
            path(&project, "nope"),
            Err(PmError::FeatureNotFound(_))
        ));
    }

    #[test]
    fn collect_keeps_the_pm_summary_over_a_legacy_one() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        write(&project, "login", "current");
        std::fs::write(project.join("login/summary.md"), "legacy").unwrap();

        let left = collect(&project, &project.join("login"), "login").unwrap();

        assert_eq!(left, Some(paths::summary_path(&project, "login")));
        assert_eq!(show(&project, "login").unwrap(), "current");
    }

    #[test]
    fn collect_without_any_summary_leaves_nothing_to_triage() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");

        let left = collect(&project, &project.join("login"), "login").unwrap();

        assert_eq!(left, None);
    }
}
