//! A project's scopes: `main` and each feature.
//!
//! Every scope has a team status ([`TeamStatus`]). A feature's lives in its
//! state file; main's in `.pm/main.toml`, kept out of `features/` so main
//! never lists as a feature. A missing `main.toml` is a working main.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;

/// The orchestrator's scope.
pub const MAIN: &str = "main";

/// Where a scope's team stands, set by its agents with `pm feat status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamStatus {
    #[serde(default)]
    pub progress: Progress,
    /// What a blocked team is waiting on the user for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    /// The agent that marked the scope blocked: the one to answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
}

impl TeamStatus {
    /// `scope`'s status; a feature that doesn't exist is an error.
    pub fn load(project_root: &Path, scope: &str) -> Result<Self> {
        if scope != MAIN {
            return Ok(FeatureState::load(&paths::features_dir(project_root), scope)?.team);
        }
        match std::fs::read_to_string(main_status_path(project_root)) {
            Ok(content) => Ok(toml::from_str(&content)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Record this as `scope`'s status. A feature's last activity is
    /// stamped with it.
    pub fn save(&self, project_root: &Path, scope: &str) -> Result<()> {
        if scope != MAIN {
            let features_dir = paths::features_dir(project_root);
            let mut state = FeatureState::load(&features_dir, scope)?;
            state.team = self.clone();
            state.last_active = chrono::Utc::now();
            return state.save(&features_dir, scope);
        }
        crate::fs_utils::write_atomic(
            &main_status_path(project_root),
            toml::to_string_pretty(self)?.as_bytes(),
        )
    }

    /// The reason, only while blocked.
    pub fn reason(&self) -> Option<&str> {
        self.blocked_reason
            .as_deref()
            .filter(|_| self.progress == Progress::Blocked)
    }

    /// The agent that blocked it, only while blocked.
    pub fn agent(&self) -> Option<&str> {
        self.blocked_by
            .as_deref()
            .filter(|_| self.progress == Progress::Blocked)
    }
}

/// Main's team status: `<project>/.pm/main.toml`.
fn main_status_path(project_root: &Path) -> PathBuf {
    paths::pm_dir(project_root).join("main.toml")
}

/// Every scope of the project at `project_root`: main, then each feature.
pub fn names(project_root: &Path) -> Result<Vec<String>> {
    let features = FeatureState::list(&paths::features_dir(project_root))?;
    Ok(std::iter::once(MAIN.to_string())
        .chain(features.into_iter().map(|(name, _)| name))
        .collect())
}

/// Whether `scope` is main or a known feature.
pub fn exists(project_root: &Path, scope: &str) -> bool {
    scope == MAIN || FeatureState::exists(&paths::features_dir(project_root), scope)
}

/// `Ok` if `scope` is main or a known feature.
pub fn check(project_root: &Path, scope: &str) -> Result<()> {
    if exists(project_root, scope) {
        Ok(())
    } else {
        Err(PmError::FeatureNotFound(scope.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn mains_status_is_wip_until_saved_and_round_trips() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(paths::pm_dir(dir.path())).unwrap();
        assert_eq!(
            TeamStatus::load(dir.path(), MAIN).unwrap(),
            TeamStatus::default()
        );

        let blocked = TeamStatus {
            progress: Progress::Blocked,
            blocked_reason: Some("next item?".into()),
            blocked_by: Some("main".into()),
        };
        blocked.save(dir.path(), MAIN).unwrap();
        assert_eq!(TeamStatus::load(dir.path(), MAIN).unwrap(), blocked);
        assert!(
            FeatureState::list(&paths::features_dir(dir.path()))
                .unwrap()
                .is_empty(),
            "main must never list as a feature"
        );
    }

    #[test]
    fn a_scope_is_main_or_a_known_feature() {
        let dir = tempdir().unwrap();
        let features_dir = paths::features_dir(dir.path());
        std::fs::create_dir_all(&features_dir).unwrap();
        std::fs::write(features_dir.join("login.toml"), "").unwrap();

        assert!(check(dir.path(), MAIN).is_ok());
        assert!(check(dir.path(), "login").is_ok());
        assert!(matches!(
            check(dir.path(), "bogus").unwrap_err(),
            PmError::FeatureNotFound(n) if n == "bogus"
        ));
    }

    #[test]
    fn a_missing_features_status_is_an_error() {
        let dir = tempdir().unwrap();
        let err = TeamStatus::load(dir.path(), "login").unwrap_err();
        assert!(matches!(err, PmError::FeatureNotFound(n) if n == "login"));
    }
}
