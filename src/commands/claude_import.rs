use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use crate::error::{PmError, Result};
use crate::state::paths;
use crate::state::project::ProjectEntry;

use super::claude_migrate::{claude_base_dir, migrate_sessions, path_to_key};

/// Import Claude Code sessions from a tarball exported by `pm claude export`.
///
/// For each project in the tarball's manifest, looks up the local registry to
/// find the local path, then copies session data and rewrites embedded paths.
///
/// Returns human-readable status messages.
pub fn import(
    tarball: &Path,
    projects_dir: &Path,
    claude_base: Option<&Path>,
) -> Result<Vec<String>> {
    let base = match claude_base {
        Some(b) => b.to_path_buf(),
        None => claude_base_dir()?,
    };

    if !tarball.exists() {
        return Err(PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("tarball not found: {}", tarball.display()),
        )));
    }

    // Extract to temp dir
    let staging = tempfile::tempdir()?;
    let status = std::process::Command::new("tar")
        .args([
            "-xzf",
            &tarball.to_string_lossy(),
            "-C",
            &staging.path().to_string_lossy(),
        ])
        .status()?;

    if !status.success() {
        return Err(PmError::ExportImport("tar extraction failed".to_string()));
    }

    let export_root = staging.path().join("pm-claude-export");
    let manifest_path = export_root.join("manifest.json");
    if !manifest_path.exists() {
        return Err(PmError::ExportImport(
            "invalid export: manifest.json not found".to_string(),
        ));
    }

    let manifest_content = std::fs::read_to_string(&manifest_path)?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_content)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    let manifest_obj = manifest
        .as_object()
        .ok_or_else(|| PmError::ExportImport("manifest.json is not an object".to_string()))?;

    let mut messages = Vec::new();
    let claude_projects = base.join("projects");
    std::fs::create_dir_all(&claude_projects)?;

    for (project_name, info) in manifest_obj {
        let old_path = info["path"].as_str().ok_or_else(|| {
            PmError::ExportImport(format!("missing 'path' for project '{project_name}'"))
        })?;
        let old_key = info["key"].as_str().ok_or_else(|| {
            PmError::ExportImport(format!("missing 'key' for project '{project_name}'"))
        })?;

        // Look up local registry
        let local_entry = match ProjectEntry::load(projects_dir, project_name) {
            Ok(entry) => entry,
            Err(_) => {
                messages.push(format!("Skipping '{project_name}': not registered locally"));
                continue;
            }
        };

        let local_root = local_entry.root_path();
        let local_main = paths::main_worktree(&local_root);
        let new_key = path_to_key(&local_main);

        // Source dir in the extracted tarball
        let src_dir = export_root.join("projects").join(old_key);
        if !src_dir.exists() {
            messages.push(format!(
                "Skipping '{project_name}': session data not found in tarball"
            ));
            continue;
        }

        let old_path_path = Path::new(old_path);
        let same_path = old_path_path == local_main;

        if same_path {
            // Same path on both machines — just copy directly
            let dst_dir = claude_projects.join(&new_key);
            if dst_dir.exists() {
                messages.push(format!(
                    "Skipping '{project_name}': Claude sessions already exist locally"
                ));
                continue;
            }
            crate::fs_utils::copy_dir_recursive(&src_dir, &dst_dir)?;
            messages.push(format!("Imported '{project_name}' (same path)"));
        } else {
            // Check if sessions already exist at the new local path
            let new_key_dir = claude_projects.join(&new_key);
            if new_key_dir.exists() {
                messages.push(format!(
                    "Skipping '{project_name}': Claude sessions already exist locally"
                ));
                continue;
            }

            // Different path — copy into claude projects with OLD key, then migrate
            let old_key_dst = claude_projects.join(old_key);
            let needs_cleanup = !old_key_dst.exists();

            if needs_cleanup {
                // Temporarily place old key dir so migrate_sessions can find it
                crate::fs_utils::copy_dir_recursive(&src_dir, &old_key_dst)?;
            }

            let migrate_msgs = migrate_sessions(old_path_path, &local_main, Some(&base))?;
            for msg in migrate_msgs {
                messages.push(format!("  {project_name}: {msg}"));
            }

            // Clean up the temporary old key dir if we created it
            if needs_cleanup && old_key_dst.exists() {
                let _ = std::fs::remove_dir_all(&old_key_dst);
            }

            messages.push(format!("Imported '{project_name}' (path rewritten)"));
        }
    }

    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::claude_export;
    use crate::state::project::{ProjectConfig, ProjectInfo};
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
            project: ProjectInfo {
                name: name.to_string(),
                max_features: None,
            },
            setup: Default::default(),
            github: Default::default(),
            agents: Default::default(),
        };
        config.save(&pm_dir).unwrap();
        let main_path = paths::main_worktree(root);
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
    fn import_same_path() {
        let claude_src = tempdir().unwrap();
        let claude_dst = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        let src_projects_dir = tempdir().unwrap();
        let dst_projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();

        // Setup source: project at a specific path
        let main_path = setup_project(project_dir.path(), "myapp", src_projects_dir.path());
        setup_claude_sessions(claude_src.path(), &main_path);

        // Export
        let output_path = output_dir.path().join("export.tar.gz");
        let (tarball, _) = claude_export::export(
            Some(project_dir.path()),
            src_projects_dir.path(),
            false,
            Some(&output_path),
            Some(claude_src.path()),
        )
        .unwrap();

        // Setup destination: same project name, SAME path
        let entry = ProjectEntry {
            root: project_dir.path().to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(dst_projects_dir.path(), "myapp").unwrap();

        // Import
        std::fs::create_dir_all(claude_dst.path().join("projects")).unwrap();
        let msgs = import(&tarball, dst_projects_dir.path(), Some(claude_dst.path())).unwrap();

        assert!(msgs.iter().any(|m| m.contains("Imported 'myapp'")));

        // Verify sessions exist at destination
        let key = path_to_key(&main_path);
        let imported_dir = claude_dst.path().join("projects").join(&key);
        assert!(imported_dir.exists());
        assert!(imported_dir.join("session.jsonl").exists());
    }

    #[test]
    fn import_different_path_rewrites() {
        let claude_src = tempdir().unwrap();
        let claude_dst = tempdir().unwrap();
        let src_project = tempdir().unwrap();
        let dst_project = tempdir().unwrap();
        let src_projects_dir = tempdir().unwrap();
        let dst_projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();

        // Source machine: project at src path
        let src_main = setup_project(src_project.path(), "myapp", src_projects_dir.path());
        setup_claude_sessions(claude_src.path(), &src_main);

        // Export
        let output_path = output_dir.path().join("export.tar.gz");
        let (tarball, _) = claude_export::export(
            Some(src_project.path()),
            src_projects_dir.path(),
            false,
            Some(&output_path),
            Some(claude_src.path()),
        )
        .unwrap();

        // Destination machine: same project name, different path
        let dst_main = setup_project(dst_project.path(), "myapp", dst_projects_dir.path());

        // Import
        std::fs::create_dir_all(claude_dst.path().join("projects")).unwrap();
        let msgs = import(&tarball, dst_projects_dir.path(), Some(claude_dst.path())).unwrap();

        assert!(msgs.iter().any(|m| m.contains("Imported 'myapp'")));
        assert!(msgs.iter().any(|m| m.contains("path rewritten")));

        // Verify sessions exist at destination with new key
        let new_key = path_to_key(&dst_main);
        let imported_dir = claude_dst.path().join("projects").join(&new_key);
        assert!(imported_dir.exists());

        // Verify paths were rewritten
        let content = std::fs::read_to_string(imported_dir.join("session.jsonl")).unwrap();
        assert!(content.contains(&dst_main.to_string_lossy().to_string()));
        assert!(!content.contains(&src_main.to_string_lossy().to_string()));
    }

    #[test]
    fn import_skips_unregistered_projects() {
        let claude_src = tempdir().unwrap();
        let claude_dst = tempdir().unwrap();
        let src_project = tempdir().unwrap();
        let src_projects_dir = tempdir().unwrap();
        let dst_projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();

        let src_main = setup_project(src_project.path(), "myapp", src_projects_dir.path());
        setup_claude_sessions(claude_src.path(), &src_main);

        let output_path = output_dir.path().join("export.tar.gz");
        let (tarball, _) = claude_export::export(
            Some(src_project.path()),
            src_projects_dir.path(),
            false,
            Some(&output_path),
            Some(claude_src.path()),
        )
        .unwrap();

        // Don't register the project on destination
        std::fs::create_dir_all(dst_projects_dir.path()).unwrap();
        std::fs::create_dir_all(claude_dst.path().join("projects")).unwrap();

        let msgs = import(&tarball, dst_projects_dir.path(), Some(claude_dst.path())).unwrap();
        assert!(msgs.iter().any(|m| m.contains("not registered locally")));
    }

    #[test]
    fn import_skips_existing_sessions() {
        let claude_src = tempdir().unwrap();
        let claude_dst = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        let src_projects_dir = tempdir().unwrap();
        let dst_projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();

        let main_path = setup_project(project_dir.path(), "myapp", src_projects_dir.path());
        setup_claude_sessions(claude_src.path(), &main_path);

        let output_path = output_dir.path().join("export.tar.gz");
        let (tarball, _) = claude_export::export(
            Some(project_dir.path()),
            src_projects_dir.path(),
            false,
            Some(&output_path),
            Some(claude_src.path()),
        )
        .unwrap();

        // Destination already has sessions at same path
        let entry = ProjectEntry {
            root: project_dir.path().to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(dst_projects_dir.path(), "myapp").unwrap();
        setup_claude_sessions(claude_dst.path(), &main_path);

        let msgs = import(&tarball, dst_projects_dir.path(), Some(claude_dst.path())).unwrap();
        assert!(msgs.iter().any(|m| m.contains("already exist locally")));
    }

    #[test]
    fn import_different_path_skips_existing_sessions() {
        let claude_src = tempdir().unwrap();
        let claude_dst = tempdir().unwrap();
        let src_project = tempdir().unwrap();
        let dst_project = tempdir().unwrap();
        let src_projects_dir = tempdir().unwrap();
        let dst_projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();

        // Source machine
        let src_main = setup_project(src_project.path(), "myapp", src_projects_dir.path());
        setup_claude_sessions(claude_src.path(), &src_main);

        // Export
        let output_path = output_dir.path().join("export.tar.gz");
        let (tarball, _) = claude_export::export(
            Some(src_project.path()),
            src_projects_dir.path(),
            false,
            Some(&output_path),
            Some(claude_src.path()),
        )
        .unwrap();

        // Destination: different path, but already has sessions at the NEW key
        let dst_main = setup_project(dst_project.path(), "myapp", dst_projects_dir.path());
        setup_claude_sessions(claude_dst.path(), &dst_main);

        let msgs = import(&tarball, dst_projects_dir.path(), Some(claude_dst.path())).unwrap();
        assert!(msgs.iter().any(|m| m.contains("already exist locally")));
    }

    #[test]
    fn import_nonexistent_tarball_errors() {
        let projects_dir = tempdir().unwrap();
        let result = import(
            Path::new("/nonexistent/tarball.tar.gz"),
            projects_dir.path(),
            None,
        );
        assert!(result.is_err());
    }
}
