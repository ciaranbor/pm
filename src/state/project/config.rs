//! The project and global config files and their sections.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::paths;

use super::HarnessConfig;

/// Project configuration stored at `<project-root>/.pm/config.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub project: ProjectInfo,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default, skip_serializing_if = "is_default")]
    pub harness: HarnessConfig,
}

pub(super) fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// Per-agent spawn settings. All maps are keyed by the agent *definition*
/// name (what the harness launches), not the display name. Present in both
/// the project and global config; see `resolve_agent_settings`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentsConfig {
    /// Per-agent permission modes, in the agent's harness's own terms
    /// (e.g. "acceptEdits") and passed through unvalidated — like `models`.
    #[serde(default)]
    pub permissions: std::collections::BTreeMap<String, String>,
    /// Per-agent models — a family alias ("opus") or a full id ("claude-opus-5")
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, String>,
    /// Per-agent harness (`Harness` string form). Unset means `claude-code`.
    #[serde(default)]
    pub harness: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_features: Option<u32>,
}

/// Global configuration stored at ~/.config/pm/config.toml.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GlobalConfig {
    #[serde(default)]
    pub project: GlobalProjectConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default, skip_serializing_if = "is_default")]
    pub harness: HarnessConfig,
    #[serde(default, skip_serializing_if = "is_default")]
    pub serve: ServeConfig,
    #[serde(default, skip_serializing_if = "is_default")]
    pub bundled: BundledConfig,
    #[serde(default, skip_serializing_if = "is_default")]
    pub upgrade: UpgradeConfig,
}

/// `[bundled.disable]`: settings for the bundled assets. Global only: the global
/// tier is the machine's, installed with no project in view.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BundledConfig {
    #[serde(default, skip_serializing_if = "is_default")]
    pub disable: BundledDisable,
}

/// `[bundled.disable]`: bundled items kept out of the global tier and its
/// harness projections (`commands::bundled_disable`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BundledDisable {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflows: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub baseline: bool,
}

/// `pm upgrade`'s settings; global only, as an upgrade sweeps every
/// project.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpgradeConfig {
    /// Restart the idle agents whose launch is stale; unset means `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_agents: Option<bool>,
}

impl UpgradeConfig {
    pub fn restarts_agents(&self) -> bool {
        self.restart_agents != Some(false)
    }
}

/// `pm serve`'s settings; global only, as the server is the machine's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServeConfig {
    /// Push-service hosts a device's subscription may name, beyond the
    /// known ones (`commands::serve::PushPolicy`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub push_hosts: Vec<String>,
    /// The loopback port `pm serve` listens on (`commands::serve::DEFAULT_PORT`
    /// when unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
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

    /// Load from the real global config dir, defaulting on any failure.
    /// Global settings are advisory — a missing or malformed file must never
    /// block a spawn.
    pub fn load_or_default() -> Self {
        paths::global_config_dir()
            .ok()
            .and_then(|dir| Self::load(&dir).ok())
            .unwrap_or_default()
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

impl ProjectConfig {
    /// Save to `<project-root>/.pm/config.toml` using atomic write.
    pub fn save(&self, pm_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(pm_dir)?;
        let path = pm_dir.join("config.toml");
        let content = toml::to_string_pretty(self)?;

        let tmp_path = pm_dir.join(".config.toml.tmp");
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, &path)?;

        Ok(())
    }

    /// Load from `<project-root>/.pm/config.toml`.
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
    fn project_config_roundtrip_toml() {
        let config = ProjectConfig {
            project: ProjectInfo {
                name: "myapp".to_string(),
                max_features: None,
            },
            agents: Default::default(),
            harness: Default::default(),
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
        assert!(config.agents.permissions.is_empty());
    }

    #[test]
    fn project_config_written_by_older_releases_still_loads() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[project]
name = "myapp"

[setup]
script = ""
restore_script = ""

[agents.harness]
qa = "opencode"

[github]
repo = ""
"#,
        )
        .unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert_eq!(config.project.name, "myapp");
        assert_eq!(config.agents.harness["qa"], "opencode");
    }

    #[test]
    fn project_config_agents_harness_roundtrip() {
        let toml_str = r#"
[project]
name = "myapp"

[agents.harness]
implementer = "claude-code"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.agents.harness.get("implementer").unwrap(),
            "claude-code"
        );
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: ProjectConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn global_config_agents_roundtrip() {
        let toml_str = r#"
[project]
max_features = 3

[agents.permissions]
implementer = "acceptEdits"

[agents.models]
reviewer = "opus"
"#;
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.project.max_features, Some(3));
        assert_eq!(
            config.agents.permissions.get("implementer").unwrap(),
            "acceptEdits"
        );
        assert_eq!(config.agents.models.get("reviewer").unwrap(), "opus");

        let dir = tempdir().unwrap();
        config.save(dir.path()).unwrap();
        assert_eq!(GlobalConfig::load(dir.path()).unwrap(), config);
    }

    #[test]
    fn global_config_without_agents_still_loads() {
        let config: GlobalConfig = toml::from_str("[project]\nmax_features = 2\n").unwrap();
        assert_eq!(config.project.max_features, Some(2));
        assert_eq!(config.agents, AgentsConfig::default());
    }

    #[test]
    fn project_config_agents_roundtrip() {
        let toml_str = r#"
[project]
name = "myapp"

[agents.permissions]
implementer = "acceptEdits"
reviewer = ""

[agents.models]
implementer = "sonnet"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.agents.permissions.get("implementer").unwrap(),
            "acceptEdits"
        );
        assert_eq!(config.agents.permissions.get("reviewer").unwrap(), "");
        assert_eq!(config.agents.models.get("implementer").unwrap(), "sonnet");

        // Roundtrip
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: ProjectConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn project_config_ignores_unknown_agents_default_field() {
        // Legacy projects may still carry the now-removed [agents].default
        // field. Loading them must succeed (silently dropping the field),
        // so `pm upgrade` can rewrite them without manual intervention.
        let toml_str = r#"
[project]
name = "myapp"

[agents]
default = "implementer"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.project.name, "myapp");
        assert!(config.agents.permissions.is_empty());
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
            agents: Default::default(),
            harness: Default::default(),
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
    fn global_config_defaults_when_missing() {
        let dir = tempdir().unwrap();
        let config = GlobalConfig::load(dir.path()).unwrap();
        assert_eq!(config, GlobalConfig::default());
        assert_eq!(config.project.max_features, None);
    }
}
