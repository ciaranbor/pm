use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};

/// Thin pointer stored in the global registry (~/.config/pm/projects/<name>.toml).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub root: String,
    #[serde(default = "default_main_branch")]
    pub main_branch: String,
    /// The project's git remote origin URL (from the main worktree).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    /// The .pm/ state repo's remote URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_remote: Option<String>,
}

fn default_main_branch() -> String {
    "main".to_string()
}

/// Project configuration stored at <project-root>/.pm/config.toml.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub project: ProjectInfo,
    #[serde(default)]
    pub setup: SetupConfig,
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentsConfig {
    /// Default agent to spawn on `feat new` (None = no auto-spawn)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Per-agent permission modes (e.g. "acceptEdits")
    #[serde(default)]
    pub permissions: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_features: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SetupConfig {
    #[serde(default)]
    pub script: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GithubConfig {
    #[serde(default)]
    pub repo: String,
}

/// Global configuration stored at ~/.config/pm/config.toml.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GlobalConfig {
    #[serde(default)]
    pub project: GlobalProjectConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GlobalProjectConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_features: Option<u32>,
}

impl GlobalConfig {
    /// Load from ~/.config/pm/config.toml. Returns default if file doesn't exist.
    pub fn load(config_dir: &Path) -> Result<Self> {
        let path = config_dir.join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }

    /// Save to ~/.config/pm/config.toml using atomic write.
    pub fn save(&self, config_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let path = config_dir.join("config.toml");
        let content = toml::to_string_pretty(self)?;

        let tmp_path = config_dir.join(".config.toml.tmp");
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, &path)?;

        Ok(())
    }
}

/// Check whether creating a new feature would exceed the configured limit.
///
/// Counts features that are not Merged or Stale (i.e. Initializing, Wip, Review, Approved).
/// Project-level `max_features` takes precedence over the global setting.
/// If neither is set, the feature count is unlimited.
pub fn check_feature_limit(project_root: &Path) -> Result<()> {
    use crate::state::feature::FeatureState;
    use crate::state::paths;

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
        // Load global limit (best-effort — if we can't find the config dir, treat as unlimited)
        paths::global_config_dir()
            .ok()
            .and_then(|dir| GlobalConfig::load(&dir).ok())
            .and_then(|c| c.project.max_features)
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

impl ProjectEntry {
    /// Resolve `root` to an absolute path, expanding `~/` if present.
    pub fn root_path(&self) -> PathBuf {
        crate::path_utils::resolve(&self.root)
    }

    /// Save to the global registry using atomic write.
    pub fn save(&self, projects_dir: &Path, name: &str) -> Result<()> {
        std::fs::create_dir_all(projects_dir)?;
        let path = projects_dir.join(format!("{name}.toml"));
        let content = toml::to_string_pretty(self)?;

        let tmp_path = projects_dir.join(format!(".{name}.toml.tmp"));
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, &path)?;

        Ok(())
    }

    /// Load from the global registry.
    pub fn load(projects_dir: &Path, name: &str) -> Result<Self> {
        let path = projects_dir.join(format!("{name}.toml"));
        if !path.exists() {
            return Err(PmError::ProjectNotFound(name.to_string()));
        }
        let content = std::fs::read_to_string(&path)?;
        let entry: Self = toml::from_str(&content)?;
        Ok(entry)
    }

    /// List all projects in the global registry. Returns (name, entry) pairs.
    pub fn list(projects_dir: &Path) -> Result<Vec<(String, Self)>> {
        if !projects_dir.exists() {
            return Ok(Vec::new());
        }

        let mut projects = Vec::new();
        for entry in std::fs::read_dir(projects_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml")
                && let Some(name) = path.file_stem().and_then(|s| s.to_str())
            {
                if name.starts_with('.') {
                    continue;
                }
                let content = std::fs::read_to_string(&path)?;
                let project: Self = toml::from_str(&content)?;
                projects.push((name.to_string(), project));
            }
        }

        projects.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(projects)
    }
}

impl ProjectConfig {
    /// Save to <project-root>/.pm/config.toml using atomic write.
    pub fn save(&self, pm_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(pm_dir)?;
        let path = pm_dir.join("config.toml");
        let content = toml::to_string_pretty(self)?;

        let tmp_path = pm_dir.join(".config.toml.tmp");
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, &path)?;

        Ok(())
    }

    /// Load from <project-root>/.pm/config.toml.
    pub fn load(pm_dir: &Path) -> Result<Self> {
        let path = pm_dir.join("config.toml");
        if !path.exists() {
            return Err(PmError::NotInProject);
        }
        let content = std::fs::read_to_string(&path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn project_entry_roundtrip_toml() {
        let entry = ProjectEntry {
            root: "/home/user/projects/myapp".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        let serialized = toml::to_string_pretty(&entry).unwrap();
        let deserialized: ProjectEntry = toml::from_str(&serialized).unwrap();
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn project_entry_roundtrip_toml_with_urls() {
        let entry = ProjectEntry {
            root: "/home/user/projects/myapp".to_string(),
            main_branch: "main".to_string(),
            repo_url: Some("https://github.com/user/myapp.git".to_string()),
            state_remote: Some("https://github.com/user/myapp-pm-state.git".to_string()),
        };
        let serialized = toml::to_string_pretty(&entry).unwrap();
        assert!(serialized.contains("repo_url"));
        assert!(serialized.contains("state_remote"));
        let deserialized: ProjectEntry = toml::from_str(&serialized).unwrap();
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn project_entry_roundtrip_none_urls_omitted() {
        let entry = ProjectEntry {
            root: "/home/user/projects/myapp".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        let serialized = toml::to_string_pretty(&entry).unwrap();
        // None fields should not appear in serialized output
        assert!(!serialized.contains("repo_url"));
        assert!(!serialized.contains("state_remote"));
    }

    #[test]
    fn project_entry_deserialize_old_format_without_url_fields() {
        // Simulates loading a TOML file from before the new fields were added
        let toml_str = r#"
root = "/home/user/projects/myapp"
main_branch = "main"
"#;
        let entry: ProjectEntry = toml::from_str(toml_str).unwrap();
        assert_eq!(entry.repo_url, None);
        assert_eq!(entry.state_remote, None);
    }

    #[test]
    fn project_entry_default_main_branch() {
        let toml_str = r#"root = "/home/user/projects/myapp""#;
        let entry: ProjectEntry = toml::from_str(toml_str).unwrap();
        assert_eq!(entry.main_branch, "main");
    }

    #[test]
    fn project_entry_save_and_load() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");

        let entry = ProjectEntry {
            root: "/home/user/projects/myapp".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "myapp").unwrap();

        let loaded = ProjectEntry::load(&projects_dir, "myapp").unwrap();
        assert_eq!(entry, loaded);
    }

    #[test]
    fn project_entry_save_creates_directory() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("nonexistent").join("projects");

        let entry = ProjectEntry {
            root: "/tmp/test".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "test").unwrap();

        assert!(projects_dir.exists());
        assert!(projects_dir.join("test.toml").exists());
    }

    #[test]
    fn project_entry_load_nonexistent_returns_error() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let result = ProjectEntry::load(&projects_dir, "nonexistent");
        assert!(matches!(result.unwrap_err(), PmError::ProjectNotFound(_)));
    }

    #[test]
    fn project_entry_list_returns_all_projects() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");

        let entry_a = ProjectEntry {
            root: "/tmp/alpha".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        let entry_b = ProjectEntry {
            root: "/tmp/beta".to_string(),
            main_branch: "develop".to_string(),
            repo_url: None,
            state_remote: None,
        };

        entry_a.save(&projects_dir, "alpha").unwrap();
        entry_b.save(&projects_dir, "beta").unwrap();

        let projects = ProjectEntry::list(&projects_dir).unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].0, "alpha");
        assert_eq!(projects[1].0, "beta");
    }

    #[test]
    fn project_entry_list_empty_directory() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let projects = ProjectEntry::list(&projects_dir).unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn project_entry_list_missing_directory() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("nonexistent");

        let projects = ProjectEntry::list(&projects_dir).unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn project_config_roundtrip_toml() {
        let config = ProjectConfig {
            project: ProjectInfo {
                name: "myapp".to_string(),
                max_features: None,
            },
            setup: SetupConfig {
                script: "setup.sh".to_string(),
            },
            github: GithubConfig {
                repo: "owner/repo".to_string(),
            },
            agents: Default::default(),
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: ProjectConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn project_config_optional_fields_default() {
        let toml_str = r#"
[project]
name = "myapp"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.project.name, "myapp");
        assert_eq!(config.setup.script, "");
        assert_eq!(config.github.repo, "");
        assert_eq!(config.agents.default, None);
        assert!(config.agents.permissions.is_empty());
    }

    #[test]
    fn project_config_agents_roundtrip() {
        let toml_str = r#"
[project]
name = "myapp"

[agents]
default = "implementer"

[agents.permissions]
implementer = "acceptEdits"
reviewer = ""
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.agents.default, Some("implementer".to_string()));
        assert_eq!(
            config.agents.permissions.get("implementer").unwrap(),
            "acceptEdits"
        );
        assert_eq!(config.agents.permissions.get("reviewer").unwrap(), "");

        // Roundtrip
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: ProjectConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn project_config_save_and_load() {
        let dir = tempdir().unwrap();
        let pm_dir = dir.path().join(".pm");

        let config = ProjectConfig {
            project: ProjectInfo {
                name: "myapp".to_string(),
                max_features: None,
            },
            setup: SetupConfig::default(),
            github: GithubConfig::default(),
            agents: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        let loaded = ProjectConfig::load(&pm_dir).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn project_info_max_features_omitted_when_none() {
        let info = ProjectInfo {
            name: "myapp".to_string(),
            max_features: None,
        };
        let serialized = toml::to_string_pretty(&info).unwrap();
        assert!(!serialized.contains("max_features"));
    }

    #[test]
    fn project_info_max_features_serialized_when_set() {
        let info = ProjectInfo {
            name: "myapp".to_string(),
            max_features: Some(3),
        };
        let serialized = toml::to_string_pretty(&info).unwrap();
        assert!(serialized.contains("max_features = 3"));
    }

    #[test]
    fn project_info_deserialize_without_max_features() {
        let toml_str = r#"name = "myapp""#;
        let info: ProjectInfo = toml::from_str(toml_str).unwrap();
        assert_eq!(info.max_features, None);
    }

    #[test]
    fn global_config_roundtrip() {
        let dir = tempdir().unwrap();
        let config = GlobalConfig {
            project: GlobalProjectConfig {
                max_features: Some(5),
            },
        };
        config.save(dir.path()).unwrap();
        let loaded = GlobalConfig::load(dir.path()).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn global_config_defaults_when_missing() {
        let dir = tempdir().unwrap();
        let config = GlobalConfig::load(dir.path()).unwrap();
        assert_eq!(config, GlobalConfig::default());
        assert_eq!(config.project.max_features, None);
    }

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
            setup: SetupConfig::default(),
            github: GithubConfig::default(),
            agents: Default::default(),
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
                created: chrono::Utc::now(),
                last_active: chrono::Utc::now(),
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
            setup: SetupConfig::default(),
            github: GithubConfig::default(),
            agents: Default::default(),
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
                created: chrono::Utc::now(),
                last_active: chrono::Utc::now(),
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
            setup: SetupConfig::default(),
            github: GithubConfig::default(),
            agents: Default::default(),
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
                created: chrono::Utc::now(),
                last_active: chrono::Utc::now(),
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
            setup: SetupConfig::default(),
            github: GithubConfig::default(),
            agents: Default::default(),
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
            created: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
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
            setup: SetupConfig::default(),
            github: GithubConfig::default(),
            agents: Default::default(),
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
            created: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
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
            setup: SetupConfig::default(),
            github: GithubConfig::default(),
            agents: Default::default(),
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
                created: chrono::Utc::now(),
                last_active: chrono::Utc::now(),
            };
            state.save(&features_dir, &format!("feat-{i}")).unwrap();
        }

        // Verify project-level limit is respected (3 < 5, so allowed)
        assert!(check_feature_limit(project_root).is_ok());
    }
}
