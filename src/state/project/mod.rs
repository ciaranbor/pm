//! Project and global configuration, and the global project registry.
//! Settings resolve project over global, per key; see each submodule.

mod agent_settings;
mod config;
mod entry;
mod harness_config;
mod presence;

use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::paths;

pub(crate) use agent_settings::layered;
pub use agent_settings::{AgentSettings, WILDCARD_AGENT, resolve_agent_settings};
pub use config::{
    AgentsConfig, BundledConfig, BundledDisable, GlobalConfig, GlobalProjectConfig, ProjectConfig,
    ProjectInfo, ServeConfig, UpgradeConfig,
};
pub use entry::{Malformed, ProjectEntry, Registry};
pub use harness_config::{
    CodexConfig, HarnessConfig, OpenCodeConfig, harness_config, harness_config_in,
    resolve_harness_config,
};
pub use presence::Presence;

/// Check whether creating a new feature would exceed the configured limit.
///
/// Counts features that are not Merged or Stale (i.e. Initializing, Wip, Review, Approved).
/// Project-level `max_features` takes precedence over the global setting.
/// If neither is set, the feature count is unlimited.
pub fn check_feature_limit(project_root: &Path) -> Result<()> {
    use crate::state::feature::FeatureState;

    let pm_dir = paths::pm_dir(project_root);
    let features_dir = paths::features_dir(project_root);

    // Load project-level limit
    let project_limit = ProjectConfig::load(&pm_dir)
        .ok()
        .and_then(|c| c.project.max_features);

    // Resolve effective limit: project overrides global
    let limit = if project_limit.is_some() {
        project_limit
    } else {
        GlobalConfig::load_or_default().project.max_features
    };

    let Some(max) = limit else {
        return Ok(()); // unlimited
    };

    // Count features that are not Merged or Stale
    use crate::state::feature::FeatureStatus;
    let features = FeatureState::list(&features_dir)?;
    let active_count = features
        .iter()
        .filter(|(_, s)| !matches!(s.status, FeatureStatus::Merged | FeatureStatus::Stale))
        .count() as u32;

    if active_count >= max {
        return Err(PmError::SafetyCheck(format!(
            "Feature limit reached ({active_count}/{max} active features). \
             Merge or delete a feature before creating new ones."
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn check_feature_limit_unlimited_when_not_set() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let pm_dir = project_root.join(".pm");
        let features_dir = pm_dir.join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        // Write config without max_features
        let config = ProjectConfig {
            project: ProjectInfo {
                name: "test".to_string(),
                max_features: None,
            },
            agents: Default::default(),
            harness: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        // Create many features — should never fail
        for i in 0..10 {
            let state = crate::state::feature::FeatureState {
                status: crate::state::feature::FeatureStatus::Wip,
                branch: format!("feat-{i}"),
                worktree: format!("feat-{i}"),
                base: String::new(),
                pr: String::new(),
                context: String::new(),
                workflow: None,
                created: chrono::Utc::now(),
                last_active: chrono::Utc::now(),
                progress: Default::default(),
                blocked_reason: None,
                blocked_by: None,
            };
            state.save(&features_dir, &format!("feat-{i}")).unwrap();
        }

        assert!(check_feature_limit(project_root).is_ok());
    }

    #[test]
    fn check_feature_limit_project_limit_enforced() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let pm_dir = project_root.join(".pm");
        let features_dir = pm_dir.join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        let config = ProjectConfig {
            project: ProjectInfo {
                name: "test".to_string(),
                max_features: Some(2),
            },
            agents: Default::default(),
            harness: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        // Create 2 Wip features
        for i in 0..2 {
            let state = crate::state::feature::FeatureState {
                status: crate::state::feature::FeatureStatus::Wip,
                branch: format!("feat-{i}"),
                worktree: format!("feat-{i}"),
                base: String::new(),
                pr: String::new(),
                context: String::new(),
                workflow: None,
                created: chrono::Utc::now(),
                last_active: chrono::Utc::now(),
                progress: Default::default(),
                blocked_reason: None,
                blocked_by: None,
            };
            state.save(&features_dir, &format!("feat-{i}")).unwrap();
        }

        let result = check_feature_limit(project_root);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, PmError::SafetyCheck(_)));
        assert!(err.to_string().contains("2/2 active features"));
    }

    #[test]
    fn check_feature_limit_under_limit_allows() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let pm_dir = project_root.join(".pm");
        let features_dir = pm_dir.join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        let config = ProjectConfig {
            project: ProjectInfo {
                name: "test".to_string(),
                max_features: Some(3),
            },
            agents: Default::default(),
            harness: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        // Create 2 features (under limit of 3)
        for i in 0..2 {
            let state = crate::state::feature::FeatureState {
                status: crate::state::feature::FeatureStatus::Wip,
                branch: format!("feat-{i}"),
                worktree: format!("feat-{i}"),
                base: String::new(),
                pr: String::new(),
                context: String::new(),
                workflow: None,
                created: chrono::Utc::now(),
                last_active: chrono::Utc::now(),
                progress: Default::default(),
                blocked_reason: None,
                blocked_by: None,
            };
            state.save(&features_dir, &format!("feat-{i}")).unwrap();
        }

        assert!(check_feature_limit(project_root).is_ok());
    }

    #[test]
    fn check_feature_limit_merged_features_not_counted() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let pm_dir = project_root.join(".pm");
        let features_dir = pm_dir.join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        let config = ProjectConfig {
            project: ProjectInfo {
                name: "test".to_string(),
                max_features: Some(1),
            },
            agents: Default::default(),
            harness: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        // Create a merged feature — should not count toward limit
        let state = crate::state::feature::FeatureState {
            status: crate::state::feature::FeatureStatus::Merged,
            branch: "old-feat".to_string(),
            worktree: "old-feat".to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
            progress: Default::default(),
            blocked_reason: None,
            blocked_by: None,
        };
        state.save(&features_dir, "old-feat").unwrap();

        assert!(check_feature_limit(project_root).is_ok());
    }

    #[test]
    fn check_feature_limit_stale_features_not_counted() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let pm_dir = project_root.join(".pm");
        let features_dir = pm_dir.join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        let config = ProjectConfig {
            project: ProjectInfo {
                name: "test".to_string(),
                max_features: Some(1),
            },
            agents: Default::default(),
            harness: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        // Create a stale feature — should not count toward limit
        let state = crate::state::feature::FeatureState {
            status: crate::state::feature::FeatureStatus::Stale,
            branch: "stale-feat".to_string(),
            worktree: "stale-feat".to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
            progress: Default::default(),
            blocked_reason: None,
            blocked_by: None,
        };
        state.save(&features_dir, "stale-feat").unwrap();

        assert!(check_feature_limit(project_root).is_ok());
    }

    #[test]
    fn check_feature_limit_uses_project_limit() {
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        let pm_dir = project_root.join(".pm");
        let features_dir = pm_dir.join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        // Project allows 5
        let config = ProjectConfig {
            project: ProjectInfo {
                name: "test".to_string(),
                max_features: Some(5),
            },
            agents: Default::default(),
            harness: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        // Create 3 features — under project limit of 5
        for i in 0..3 {
            let state = crate::state::feature::FeatureState {
                status: crate::state::feature::FeatureStatus::Wip,
                branch: format!("feat-{i}"),
                worktree: format!("feat-{i}"),
                base: String::new(),
                pr: String::new(),
                context: String::new(),
                workflow: None,
                created: chrono::Utc::now(),
                last_active: chrono::Utc::now(),
                progress: Default::default(),
                blocked_reason: None,
                blocked_by: None,
            };
            state.save(&features_dir, &format!("feat-{i}")).unwrap();
        }

        // Verify project-level limit is respected (3 < 5, so allowed)
        assert!(check_feature_limit(project_root).is_ok());
    }
}
