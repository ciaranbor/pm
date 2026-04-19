use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::state::project::{ProjectConfig, ProjectEntry};

use super::claude_migrate::{claude_base_dir, path_to_key};

/// Build a manifest mapping project names to their original paths and Claude keys.
fn build_manifest(projects: &[(String, PathBuf)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (name, path) in projects {
        let path_str = path.to_string_lossy().to_string();
        let key = path_to_key(path);
        map.insert(
            name.clone(),
            serde_json::json!({
                "path": path_str,
                "key": key,
            }),
        );
    }
    serde_json::Value::Object(map)
}

/// Resolve which projects to export and their main worktree paths.
fn resolve_projects(
    project_root: Option<&Path>,
    projects_dir: &Path,
    all: bool,
) -> Result<Vec<(String, PathBuf)>> {
    if all {
        let entries = ProjectEntry::list(projects_dir)?;
        if entries.is_empty() {
            return Err(PmError::ExportImport("no projects registered".to_string()));
        }
        Ok(entries
            .into_iter()
            .map(|(name, entry)| {
                let root = entry.root_path();
                let main_path = root.join("main");
                (name, main_path)
            })
            .collect())
    } else {
        let root = project_root.ok_or(PmError::NotInProject)?;
        let pm_dir = root.join(".pm");
        let config = ProjectConfig::load(&pm_dir)?;
        let main_path = root.join("main");
        Ok(vec![(config.project.name, main_path)])
    }
}

/// Export Claude Code sessions for one or all projects into a tarball.
///
/// Returns the path to the created tarball and a list of status messages.
pub fn export(
    project_root: Option<&Path>,
    projects_dir: &Path,
    all: bool,
    output: Option<&Path>,
    claude_base: Option<&Path>,
) -> Result<(PathBuf, Vec<String>)> {
    let base = match claude_base {
        Some(b) => b.to_path_buf(),
        None => claude_base_dir()?,
    };
    let claude_projects = base.join("projects");

    let projects = resolve_projects(project_root, projects_dir, all)?;
    let mut messages = Vec::new();

    // Determine which project dirs actually exist in ~/.claude/projects/
    let mut exportable: Vec<(String, PathBuf)> = Vec::new();
    for (name, path) in &projects {
        let key = path_to_key(path);
        let dir = claude_projects.join(&key);
        if dir.exists() {
            exportable.push((name.clone(), path.clone()));
        } else {
            messages.push(format!("Skipping '{name}': no Claude sessions found"));
        }
    }

    if exportable.is_empty() {
        return Err(PmError::ExportImport(
            "no Claude sessions found for any project".to_string(),
        ));
    }

    // Create a staging directory
    let staging = tempfile::tempdir()?;
    let staging_root = staging.path().join("pm-claude-export");
    let staging_projects = staging_root.join("projects");
    std::fs::create_dir_all(&staging_projects)?;

    // Write manifest
    let manifest = build_manifest(&exportable);
    std::fs::write(
        staging_root.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
    )?;

    // Copy project dirs into staging
    for (name, path) in &exportable {
        let key = path_to_key(path);
        let src = claude_projects.join(&key);
        let dst = staging_projects.join(&key);
        crate::fs_utils::copy_dir_recursive(&src, &dst)?;
        messages.push(format!("Exported '{name}' ({key})"));
    }

    // Determine output path
    let output_path = match output {
        Some(p) => p.to_path_buf(),
        None => {
            let name = if exportable.len() == 1 {
                format!("pm-claude-{}.tar.gz", exportable[0].0)
            } else {
                "pm-claude-export.tar.gz".to_string()
            };
            std::env::current_dir()?.join(name)
        }
    };

    // Create tarball
    let status = std::process::Command::new("tar")
        .args([
            "-czf",
            &output_path.to_string_lossy(),
            "-C",
            &staging.path().to_string_lossy(),
            "pm-claude-export",
        ])
        .status()?;

    if !status.success() {
        return Err(PmError::ExportImport("tar command failed".to_string()));
    }

    messages.push(format!("Created {}", output_path.display()));
    Ok((output_path, messages))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_claude_sessions(base: &Path, project_path: &Path) {
        let key = path_to_key(project_path);
        let dir = base.join("projects").join(&key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session.jsonl"),
            format!("{{\"cwd\":\"{}\"}}\n", project_path.display()),
        )
        .unwrap();
        std::fs::write(
            dir.join("sessions-index.json"),
            format!(
                "[{{\"sessionId\":\"abc\",\"fullPath\":\"{}\"}}]",
                project_path.display()
            ),
        )
        .unwrap();
    }

    fn setup_project(root: &Path, name: &str, projects_dir: &Path) -> PathBuf {
        let pm_dir = root.join(".pm");
        std::fs::create_dir_all(&pm_dir).unwrap();
        let config = ProjectConfig {
            project: crate::state::project::ProjectInfo {
                name: name.to_string(),
            },
            setup: Default::default(),
            github: Default::default(),
            agents: Default::default(),
        };
        config.save(&pm_dir).unwrap();
        let main_path = root.join("main");
        std::fs::create_dir_all(&main_path).unwrap();

        let entry = ProjectEntry {
            root: root.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(projects_dir, name).unwrap();
        main_path
    }

    #[test]
    fn export_single_project() {
        let claude_base = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();

        let main_path = setup_project(project_dir.path(), "myapp", projects_dir.path());
        setup_claude_sessions(claude_base.path(), &main_path);

        let output_path = output_dir.path().join("export.tar.gz");
        let (path, msgs) = export(
            Some(project_dir.path()),
            projects_dir.path(),
            false,
            Some(&output_path),
            Some(claude_base.path()),
        )
        .unwrap();

        assert_eq!(path, output_path);
        assert!(output_path.exists());
        assert!(msgs.iter().any(|m| m.contains("Exported 'myapp'")));
        assert!(msgs.iter().any(|m| m.contains("Created")));
    }

    #[test]
    fn export_all_projects() {
        let claude_base = tempdir().unwrap();
        let project_a = tempdir().unwrap();
        let project_b = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();

        let main_a = setup_project(project_a.path(), "alpha", projects_dir.path());
        let main_b = setup_project(project_b.path(), "beta", projects_dir.path());
        setup_claude_sessions(claude_base.path(), &main_a);
        setup_claude_sessions(claude_base.path(), &main_b);

        let output_path = output_dir.path().join("all.tar.gz");
        let (_, msgs) = export(
            None,
            projects_dir.path(),
            true,
            Some(&output_path),
            Some(claude_base.path()),
        )
        .unwrap();

        assert!(output_path.exists());
        assert!(msgs.iter().any(|m| m.contains("Exported 'alpha'")));
        assert!(msgs.iter().any(|m| m.contains("Exported 'beta'")));
    }

    #[test]
    fn export_skips_missing_sessions() {
        let claude_base = tempdir().unwrap();
        let project_a = tempdir().unwrap();
        let project_b = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();

        let main_a = setup_project(project_a.path(), "alpha", projects_dir.path());
        setup_project(project_b.path(), "beta", projects_dir.path());
        // Only alpha has sessions
        setup_claude_sessions(claude_base.path(), &main_a);

        let output_path = output_dir.path().join("partial.tar.gz");
        let (_, msgs) = export(
            None,
            projects_dir.path(),
            true,
            Some(&output_path),
            Some(claude_base.path()),
        )
        .unwrap();

        assert!(msgs.iter().any(|m| m.contains("Skipping 'beta'")));
        assert!(msgs.iter().any(|m| m.contains("Exported 'alpha'")));
    }

    #[test]
    fn export_errors_when_no_sessions_found() {
        let claude_base = tempdir().unwrap();
        std::fs::create_dir_all(claude_base.path().join("projects")).unwrap();
        let project_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();

        setup_project(project_dir.path(), "empty", projects_dir.path());

        let result = export(
            Some(project_dir.path()),
            projects_dir.path(),
            false,
            None,
            Some(claude_base.path()),
        );
        assert!(result.is_err());
    }

    #[test]
    fn manifest_structure() {
        let projects = vec![(
            "myapp".to_string(),
            PathBuf::from("/Users/test/projects/myapp/main"),
        )];
        let manifest = build_manifest(&projects);
        let entry = manifest.get("myapp").unwrap();
        assert_eq!(entry["path"], "/Users/test/projects/myapp/main");
        assert_eq!(entry["key"], "-Users-test-projects-myapp-main");
    }
}
