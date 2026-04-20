use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Agent,
    User,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEntry {
    #[serde(rename = "type")]
    pub agent_type: AgentType,
    #[serde(default)]
    pub session_id: String,
    /// The tmux window name used to locate this agent's window via
    /// `tmux::find_window`. Duplicates the registry key today, but is
    /// stored explicitly so the TOML file is self-describing and the
    /// lookup contract doesn't silently depend on the map key.
    #[serde(default)]
    pub window_name: String,
}

/// Agent registry for a feature. Stored at `.pm/agents/<feature>.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentRegistry {
    #[serde(default)]
    pub agents: BTreeMap<String, AgentEntry>,
}

impl AgentRegistry {
    /// Load the agent registry for a feature.
    pub fn load(agents_dir: &Path, feature: &str) -> Result<Self> {
        let path = agents_dir.join(format!("{feature}.toml"));
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        let registry: Self = toml::from_str(&content)?;
        Ok(registry)
    }

    /// Save the agent registry for a feature.
    pub fn save(&self, agents_dir: &Path, feature: &str) -> Result<()> {
        std::fs::create_dir_all(agents_dir)?;
        let path = agents_dir.join(format!("{feature}.toml"));
        let content = toml::to_string_pretty(self)?;

        let tmp = agents_dir.join(format!(".{feature}.toml.tmp"));
        std::fs::write(&tmp, &content)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Register or update an agent.
    pub fn register(&mut self, name: &str, entry: AgentEntry) {
        self.agents.insert(name.to_string(), entry);
    }

    /// Get an agent entry by name.
    pub fn get(&self, name: &str) -> Option<&AgentEntry> {
        self.agents.get(name)
    }

    /// Get a mutable agent entry by name.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut AgentEntry> {
        self.agents.get_mut(name)
    }

    /// List all agent names.
    pub fn names(&self) -> Vec<&str> {
        self.agents.keys().map(|s| s.as_str()).collect()
    }

    /// Delete the agent registry file for a feature. No-op if missing.
    pub fn delete(agents_dir: &Path, feature: &str) -> Result<()> {
        let path = agents_dir.join(format!("{feature}.toml"));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_agent(session_id: &str) -> AgentEntry {
        AgentEntry {
            agent_type: AgentType::Agent,
            session_id: session_id.to_string(),
            window_name: "reviewer".to_string(),
        }
    }

    #[test]
    fn registry_save_and_load() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join("agents");

        let mut registry = AgentRegistry::default();
        registry.register("reviewer", make_agent("abc123"));

        registry.save(&agents_dir, "login").unwrap();

        let loaded = AgentRegistry::load(&agents_dir, "login").unwrap();
        assert_eq!(registry, loaded);
    }

    #[test]
    fn registry_load_missing_returns_default() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join("agents");

        let registry = AgentRegistry::load(&agents_dir, "nonexistent").unwrap();
        assert!(registry.agents.is_empty());
    }

    #[test]
    fn registry_get_and_update() {
        let mut registry = AgentRegistry::default();
        registry.register("reviewer", make_agent("abc"));

        assert_eq!(registry.get("reviewer").unwrap().session_id, "abc");

        registry.get_mut("reviewer").unwrap().session_id = "def".to_string();
        assert_eq!(registry.get("reviewer").unwrap().session_id, "def");
    }

    #[test]
    fn registry_names_sorted() {
        let mut registry = AgentRegistry::default();
        registry.register("reviewer", make_agent("a"));
        registry.register("implementer", make_agent("b"));

        let names = registry.names();
        assert_eq!(names, vec!["implementer", "reviewer"]);
    }

    #[test]
    fn registry_user_type() {
        let mut registry = AgentRegistry::default();
        registry.register(
            "ciaranorourke",
            AgentEntry {
                agent_type: AgentType::User,
                session_id: String::new(),
                window_name: String::new(),
            },
        );

        let entry = registry.get("ciaranorourke").unwrap();
        assert_eq!(entry.agent_type, AgentType::User);
    }

    #[test]
    fn registry_delete_removes_file() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join("agents");

        let mut registry = AgentRegistry::default();
        registry.register("reviewer", make_agent("abc123"));
        registry.save(&agents_dir, "login").unwrap();

        assert!(agents_dir.join("login.toml").exists());
        AgentRegistry::delete(&agents_dir, "login").unwrap();
        assert!(!agents_dir.join("login.toml").exists());
    }

    #[test]
    fn registry_delete_missing_is_ok() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join("agents");

        // Should not error when file doesn't exist
        AgentRegistry::delete(&agents_dir, "nonexistent").unwrap();
    }

    #[test]
    fn registry_toml_roundtrip() {
        let mut registry = AgentRegistry::default();
        registry.register("reviewer", make_agent("abc123"));
        registry.register(
            "ciaranorourke",
            AgentEntry {
                agent_type: AgentType::User,
                session_id: String::new(),
                window_name: String::new(),
            },
        );

        let toml = toml::to_string_pretty(&registry).unwrap();
        let parsed: AgentRegistry = toml::from_str(&toml).unwrap();
        assert_eq!(registry, parsed);
    }
}
