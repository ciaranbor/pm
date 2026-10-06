//! The global registry: one thin pointer per project.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::state::paths;

use super::ProjectConfig;

/// Thin pointer stored in the global registry (`~/.config/pm/projects/<name>.toml`).
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

impl ProjectEntry {
    /// The main branch recorded for the project rooted at `project_root`.
    pub fn main_branch(project_root: &Path, projects_dir: &Path) -> Result<String> {
        let name = ProjectConfig::load(&paths::pm_dir(project_root))?
            .project
            .name;
        Ok(Self::load(projects_dir, &name)?.main_branch)
    }

    /// Resolve `root` to an absolute path, expanding `~/` if present.
    pub fn root_path(&self) -> PathBuf {
        crate::path_utils::resolve(&self.root)
    }

    /// Save to the global registry using atomic write.
    ///
    /// Errors if `root` is not a portable path (absolute or `~/…`). Relative
    /// paths in the registry resolve against each caller's CWD on every load,
    /// silently corrupting cross-project operations like messaging.
    pub fn save(&self, projects_dir: &Path, name: &str) -> Result<()> {
        if !crate::path_utils::is_portable(&self.root) {
            return Err(PmError::InvalidProjectRoot(self.root.clone()));
        }

        std::fs::create_dir_all(projects_dir)?;
        let path = projects_dir.join(format!("{name}.toml"));
        let content = toml::to_string_pretty(self)?;

        let tmp_path = projects_dir.join(format!(".{name}.toml.tmp"));
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, &path)?;

        Ok(())
    }

    /// Refuse `name` for the project at `root` when the registry has a
    /// project of that name elsewhere: saving would re-point that entry,
    /// and the two projects would share their tmux sessions.
    pub fn ensure_name_free(projects_dir: &Path, name: &str, root: &Path) -> Result<()> {
        match Self::load(projects_dir, name) {
            Ok(existing) if existing.root_path() != root => Err(PmError::ProjectNameTaken {
                name: name.to_string(),
                root: existing.root_path(),
            }),
            Ok(_) | Err(PmError::ProjectNotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
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
    /// An entry that can't be read is skipped with a warning on stderr, so one
    /// bad file doesn't fail every all-project command.
    pub fn list(projects_dir: &Path) -> Result<Vec<(String, Self)>> {
        let registry = Self::scan(projects_dir)?;
        for bad in &registry.malformed {
            eprintln!(
                "warning: skipping registry entry {}: {}",
                bad.path.display(),
                bad.error
            );
        }
        Ok(registry.projects)
    }

    /// [`list`](Self::list) without the warnings: the unreadable entries are
    /// returned instead, for a caller that reports them itself or must not
    /// print.
    pub fn scan(projects_dir: &Path) -> Result<Registry> {
        let mut registry = Registry::default();
        if !projects_dir.exists() {
            return Ok(registry);
        }

        for entry in std::fs::read_dir(projects_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml")
                && let Some(name) = path.file_stem().and_then(|s| s.to_str())
            {
                if name.starts_with('.') {
                    continue;
                }
                let read = std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|content| {
                        toml::from_str::<Self>(&content).map_err(|e| parse_error(&content, &e))
                    });
                match read {
                    Ok(project) => registry.projects.push((name.to_string(), project)),
                    Err(error) => registry.malformed.push(Malformed {
                        name: name.to_string(),
                        path,
                        error,
                    }),
                }
            }
        }

        registry.projects.sort_by(|a, b| a.0.cmp(&b.0));
        registry.malformed.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(registry)
    }
}

/// `error` on one line, where toml's own rendering quotes the source over
/// several.
fn parse_error(content: &str, error: &toml::de::Error) -> String {
    let message = error.message().trim_end().replace('\n', ", ");
    match error.span() {
        Some(span) => {
            let line = 1 + content[..span.start.min(content.len())]
                .matches('\n')
                .count();
            format!("line {line}: {message}")
        }
        None => message,
    }
}

/// The global registry as [`ProjectEntry::scan`] read it.
#[derive(Debug, Default)]
pub struct Registry {
    pub projects: Vec<(String, ProjectEntry)>,
    pub malformed: Vec<Malformed>,
}

/// A registry entry that could not be read or parsed.
#[derive(Debug)]
pub struct Malformed {
    pub name: String,
    pub path: PathBuf,
    pub error: String,
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
    fn project_entry_save_rejects_relative_root() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");

        let entry = ProjectEntry {
            root: "exo-bench".to_string(), // relative — would corrupt registry
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        let err = entry.save(&projects_dir, "exo-bench").unwrap_err();
        assert!(matches!(err, PmError::InvalidProjectRoot(_)));
        assert!(err.to_string().contains("absolute"));
        // No file should have been written
        assert!(!projects_dir.join("exo-bench.toml").exists());
    }

    #[test]
    fn project_entry_save_rejects_relative_with_dot_root() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");

        let entry = ProjectEntry {
            root: "./exo-bench".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        assert!(matches!(
            entry.save(&projects_dir, "exo-bench").unwrap_err(),
            PmError::InvalidProjectRoot(_)
        ));
    }

    #[test]
    fn project_entry_save_accepts_tilde_root() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");

        let entry = ProjectEntry {
            root: "~/Projects/myapp".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "myapp").unwrap();
        assert!(projects_dir.join("myapp.toml").exists());
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
    fn project_entry_list_skips_an_unreadable_entry() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let entry = ProjectEntry {
            root: "/tmp/alpha".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "alpha").unwrap();
        let bad = projects_dir.join("beta.toml");
        std::fs::write(&bad, "root = \"/tmp/beta\"\nmain_branch = 3\n").unwrap();

        let registry = ProjectEntry::scan(&projects_dir).unwrap();
        let names: Vec<&str> = registry.projects.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["alpha"]);
        assert_eq!(registry.malformed.len(), 1);
        let malformed = &registry.malformed[0];
        assert_eq!((malformed.name.as_str(), &malformed.path), ("beta", &bad));
        assert!(
            malformed.error.starts_with("line 2: ") && !malformed.error.contains('\n'),
            "{}",
            malformed.error
        );

        let listed = ProjectEntry::list(&projects_dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, "alpha");
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
}
