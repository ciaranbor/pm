use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeatureStatus {
    Initializing,
    Wip,
    Review,
    Approved,
    Merged,
    Stale,
}

impl FeatureStatus {
    /// Whether this feature status represents an active feature that should
    /// have a tmux session.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Wip | Self::Review | Self::Approved)
    }
}

impl std::fmt::Display for FeatureStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Initializing => write!(f, "initializing"),
            Self::Wip => write!(f, "wip"),
            Self::Review => write!(f, "review"),
            Self::Approved => write!(f, "approved"),
            Self::Merged => write!(f, "merged"),
            Self::Stale => write!(f, "stale"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureState {
    pub status: FeatureStatus,
    pub branch: String,
    pub worktree: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub pr: String,
    #[serde(default)]
    pub context: String,
    /// Active workflow for this feature, referencing a directory under
    /// `<project>/.pm/workflows/<name>/`. `None` means no workflow is
    /// active (legacy features or those created without `--workflow`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    pub created: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}

impl FeatureState {
    /// The branch this feature stacks on. Features created before `base` was
    /// recorded have it empty; they were all based on the main branch.
    pub fn base_branch<'a>(&'a self, main_branch: &'a str) -> &'a str {
        if self.base.is_empty() {
            main_branch
        } else {
            &self.base
        }
    }

    /// Save the feature state to `.pm/features/<name>.toml` using atomic write.
    pub fn save(&self, features_dir: &Path, name: &str) -> Result<()> {
        std::fs::create_dir_all(features_dir)?;
        let path = features_dir.join(format!("{name}.toml"));
        let content = toml::to_string_pretty(self)?;

        // Atomic write: write to temp file, then rename
        let tmp_path = features_dir.join(format!(".{name}.toml.tmp"));
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, &path)?;

        Ok(())
    }

    /// Load a feature state from `.pm/features/<name>.toml`.
    pub fn load(features_dir: &Path, name: &str) -> Result<Self> {
        let path = features_dir.join(format!("{name}.toml"));
        if !path.exists() {
            return Err(PmError::FeatureNotFound(name.to_string()));
        }
        let content = std::fs::read_to_string(&path)?;
        let state: Self = toml::from_str(&content)?;
        Ok(state)
    }

    /// List all features in the features directory. Returns (name, state) pairs.
    pub fn list(features_dir: &Path) -> Result<Vec<(String, Self)>> {
        if !features_dir.exists() {
            return Ok(Vec::new());
        }

        let mut features = Vec::new();
        for entry in std::fs::read_dir(features_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml")
                && let Some(name) = path.file_stem().and_then(|s| s.to_str())
            {
                // Skip temp files
                if name.starts_with('.') {
                    continue;
                }
                let content = std::fs::read_to_string(&path)?;
                let state: Self = toml::from_str(&content)?;
                features.push((name.to_string(), state));
            }
        }

        features.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(features)
    }

    /// Delete a feature state file.
    pub fn delete(features_dir: &Path, name: &str) -> Result<()> {
        let path = features_dir.join(format!("{name}.toml"));
        if !path.exists() {
            return Err(PmError::FeatureNotFound(name.to_string()));
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }

    /// Check if a feature exists.
    pub fn exists(features_dir: &Path, name: &str) -> bool {
        features_dir.join(format!("{name}.toml")).exists()
    }

    /// Get the path to the feature state file.
    pub fn path(features_dir: &Path, name: &str) -> PathBuf {
        features_dir.join(format!("{name}.toml"))
    }
}

/// Where a base branch is checked out within the project: the scope whose
/// session pm returns to after a feature is torn down, and the worktree pm
/// merges into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseCheckout {
    /// `main`, or the name of the feature whose branch is `base`.
    pub scope: String,
    pub worktree: PathBuf,
}

impl BaseCheckout {
    /// The main scope's checkout.
    pub fn main(project_root: &Path) -> Self {
        Self {
            scope: "main".to_string(),
            worktree: crate::state::paths::main_worktree(project_root),
        }
    }
}

/// Locate the checkout of `base` (see [`BaseCheckout`]). `main_branch` is
/// the registry's; anything else must be the branch of a known feature,
/// which may not share its name (`feat/x` lives in worktree `feat-x`).
pub fn base_checkout(project_root: &Path, main_branch: &str, base: &str) -> Result<BaseCheckout> {
    if base == main_branch {
        return Ok(BaseCheckout::main(project_root));
    }
    let features_dir = crate::state::paths::features_dir(project_root);
    FeatureState::list(&features_dir)?
        .into_iter()
        .find(|(_, state)| state.branch == base)
        .map(|(name, state)| BaseCheckout {
            scope: name,
            worktree: project_root.join(&state.worktree),
        })
        .ok_or_else(|| PmError::BaseNotCheckedOut(base.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn base_branch_falls_back_to_the_main_branch_when_unrecorded() {
        let mut state = make_feature(FeatureStatus::Wip);
        state.base = String::new();
        assert_eq!(state.base_branch("master"), "master");
        state.base = "parent".to_string();
        assert_eq!(state.base_branch("master"), "parent");
    }

    #[test]
    fn base_checkout_maps_main_branch_to_main_scope() {
        let dir = tempdir().unwrap();
        let checkout = base_checkout(dir.path(), "master", "master").unwrap();
        assert_eq!(checkout.scope, "main");
        assert_eq!(checkout.worktree, dir.path().join("main"));
    }

    #[test]
    fn base_checkout_finds_a_feature_by_branch_not_name() {
        let dir = tempdir().unwrap();
        let features_dir = crate::state::paths::features_dir(dir.path());
        let mut state = make_feature(FeatureStatus::Wip);
        state.branch = "feat/x".to_string();
        state.worktree = "feat-x".to_string();
        state.save(&features_dir, "feat-x").unwrap();

        let checkout = base_checkout(dir.path(), "main", "feat/x").unwrap();
        assert_eq!(checkout.scope, "feat-x");
        assert_eq!(checkout.worktree, dir.path().join("feat-x"));
    }

    #[test]
    fn base_checkout_errors_when_the_branch_has_no_checkout() {
        let dir = tempdir().unwrap();
        // A `main` branch in a master-default repo is not the main scope.
        let err = base_checkout(dir.path(), "master", "main").unwrap_err();
        assert!(matches!(err, PmError::BaseNotCheckedOut(b) if b == "main"));
    }

    fn make_feature(status: FeatureStatus) -> FeatureState {
        FeatureState {
            status,
            branch: "login".to_string(),
            worktree: "login".to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: Utc::now(),
            last_active: Utc::now(),
        }
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct StatusWrapper {
        status: FeatureStatus,
    }

    #[test]
    fn feature_status_serializes_to_lowercase() {
        for (status, expected) in [
            (FeatureStatus::Wip, "wip"),
            (FeatureStatus::Initializing, "initializing"),
            (FeatureStatus::Review, "review"),
            (FeatureStatus::Approved, "approved"),
            (FeatureStatus::Merged, "merged"),
            (FeatureStatus::Stale, "stale"),
        ] {
            let wrapper = StatusWrapper { status };
            let serialized = toml::to_string(&wrapper).unwrap();
            assert!(
                serialized.contains(&format!("status = \"{expected}\"")),
                "expected status = \"{expected}\" in:\n{serialized}"
            );
        }
    }

    #[test]
    fn feature_status_deserializes_from_lowercase() {
        for (toml_str, expected) in [
            ("status = \"wip\"", FeatureStatus::Wip),
            ("status = \"initializing\"", FeatureStatus::Initializing),
            ("status = \"review\"", FeatureStatus::Review),
            ("status = \"approved\"", FeatureStatus::Approved),
            ("status = \"merged\"", FeatureStatus::Merged),
            ("status = \"stale\"", FeatureStatus::Stale),
        ] {
            let wrapper: StatusWrapper = toml::from_str(toml_str).unwrap();
            assert_eq!(wrapper.status, expected);
        }
    }

    #[test]
    fn feature_status_is_active() {
        assert!(FeatureStatus::Wip.is_active());
        assert!(FeatureStatus::Review.is_active());
        assert!(FeatureStatus::Approved.is_active());
        assert!(!FeatureStatus::Initializing.is_active());
        assert!(!FeatureStatus::Merged.is_active());
        assert!(!FeatureStatus::Stale.is_active());
    }

    #[test]
    fn feature_state_roundtrip_toml() {
        let state = make_feature(FeatureStatus::Wip);
        let serialized = toml::to_string_pretty(&state).unwrap();
        let deserialized: FeatureState = toml::from_str(&serialized).unwrap();
        assert_eq!(state, deserialized);
    }

    #[test]
    fn feature_state_save_and_load() {
        let dir = tempdir().unwrap();
        let features_dir = dir.path().join("features");

        let state = make_feature(FeatureStatus::Wip);
        state.save(&features_dir, "login").unwrap();

        let loaded = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state, loaded);
    }

    #[test]
    fn feature_state_save_creates_directory() {
        let dir = tempdir().unwrap();
        let features_dir = dir.path().join("nonexistent").join("features");

        let state = make_feature(FeatureStatus::Wip);
        state.save(&features_dir, "login").unwrap();

        assert!(features_dir.exists());
        assert!(features_dir.join("login.toml").exists());
    }

    #[test]
    fn feature_state_save_is_atomic() {
        let dir = tempdir().unwrap();
        let features_dir = dir.path().join("features");

        let state = make_feature(FeatureStatus::Wip);
        state.save(&features_dir, "login").unwrap();

        // The final file should exist
        assert!(features_dir.join("login.toml").exists());
        // The temp file should not exist
        assert!(!features_dir.join(".login.toml.tmp").exists());
    }

    #[test]
    fn feature_state_load_nonexistent_returns_error() {
        let dir = tempdir().unwrap();
        let features_dir = dir.path().join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        let result = FeatureState::load(&features_dir, "nonexistent");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn feature_state_list_returns_all_features() {
        let dir = tempdir().unwrap();
        let features_dir = dir.path().join("features");

        let state_a = make_feature(FeatureStatus::Wip);
        let state_b = make_feature(FeatureStatus::Review);

        state_a.save(&features_dir, "alpha").unwrap();
        state_b.save(&features_dir, "beta").unwrap();

        let features = FeatureState::list(&features_dir).unwrap();
        assert_eq!(features.len(), 2);
        assert_eq!(features[0].0, "alpha");
        assert_eq!(features[1].0, "beta");
        assert_eq!(features[0].1.status, FeatureStatus::Wip);
        assert_eq!(features[1].1.status, FeatureStatus::Review);
    }

    #[test]
    fn feature_state_list_empty_directory() {
        let dir = tempdir().unwrap();
        let features_dir = dir.path().join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        let features = FeatureState::list(&features_dir).unwrap();
        assert!(features.is_empty());
    }

    #[test]
    fn feature_state_list_missing_directory() {
        let dir = tempdir().unwrap();
        let features_dir = dir.path().join("nonexistent");

        let features = FeatureState::list(&features_dir).unwrap();
        assert!(features.is_empty());
    }

    #[test]
    fn feature_state_delete_removes_file() {
        let dir = tempdir().unwrap();
        let features_dir = dir.path().join("features");

        let state = make_feature(FeatureStatus::Wip);
        state.save(&features_dir, "login").unwrap();
        assert!(FeatureState::exists(&features_dir, "login"));

        FeatureState::delete(&features_dir, "login").unwrap();
        assert!(!FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn feature_state_delete_nonexistent_returns_error() {
        let dir = tempdir().unwrap();
        let features_dir = dir.path().join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        let result = FeatureState::delete(&features_dir, "nonexistent");
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn feature_state_exists_checks_file() {
        let dir = tempdir().unwrap();
        let features_dir = dir.path().join("features");

        assert!(!FeatureState::exists(&features_dir, "login"));

        let state = make_feature(FeatureStatus::Wip);
        state.save(&features_dir, "login").unwrap();
        assert!(FeatureState::exists(&features_dir, "login"));
    }
}
