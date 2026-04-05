use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{PmError, Result};

/// Thin pointer stored in the global registry (~/.config/pm/projects/<name>.toml).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub root: String,
    #[serde(default = "default_main_branch")]
    pub main_branch: String,
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
    /// Default agent to spawn on `feat new` (empty = no auto-spawn)
    #[serde(default)]
    pub default: String,
    /// Per-agent permission modes (e.g. "acceptEdits")
    #[serde(default)]
    pub permissions: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
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

impl ProjectEntry {
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
        };
        let serialized = toml::to_string_pretty(&entry).unwrap();
        let deserialized: ProjectEntry = toml::from_str(&serialized).unwrap();
        assert_eq!(entry, deserialized);
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
        };
        let entry_b = ProjectEntry {
            root: "/tmp/beta".to_string(),
            main_branch: "develop".to_string(),
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
        assert_eq!(config.agents.default, "");
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
        assert_eq!(config.agents.default, "implementer");
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
            },
            setup: SetupConfig::default(),
            github: GithubConfig::default(),
            agents: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        let loaded = ProjectConfig::load(&pm_dir).unwrap();
        assert_eq!(config, loaded);
    }
}
