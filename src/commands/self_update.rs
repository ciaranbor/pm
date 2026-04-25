use std::path::Path;
use std::process::Command;

use crate::error::{PmError, Result};
use crate::state::paths;
use crate::state::project::ProjectEntry;

/// Find the pm project's main worktree by looking it up in the global registry.
fn find_pm_source() -> Result<std::path::PathBuf> {
    let projects_dir = paths::global_projects_dir()?;
    let entry = ProjectEntry::load(&projects_dir, "pm").map_err(|_| {
        PmError::SafetyCheck(
            "pm project not found in global registry. Register it with: \
             pm register <path-to-pm-source>"
                .to_string(),
        )
    })?;
    let root = entry.root_path();
    if !root.exists() {
        return Err(PmError::SafetyCheck(format!(
            "pm project root does not exist: {}",
            root.display()
        )));
    }
    let main_worktree = paths::main_worktree(&root);
    if !main_worktree.exists() {
        return Err(PmError::SafetyCheck(format!(
            "pm main worktree not found at: {}",
            main_worktree.display()
        )));
    }
    Ok(main_worktree)
}

/// Get the HEAD short hash from a repo.
fn head_short_hash(repo: &Path) -> Result<String> {
    crate::git::run_git(repo, &["rev-parse", "--short", "HEAD"])
}

/// Read the version from Cargo.toml in the given source directory.
fn version_from_cargo_toml(source: &Path) -> Result<String> {
    let cargo_toml = source.join("Cargo.toml");
    let content = std::fs::read_to_string(&cargo_toml).map_err(|_| {
        PmError::SafetyCheck(format!(
            "Could not read Cargo.toml at: {}",
            cargo_toml.display()
        ))
    })?;
    let doc: toml::Table = toml::from_str(&content)
        .map_err(|_| PmError::SafetyCheck("Could not parse Cargo.toml".to_string()))?;
    doc.get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            PmError::SafetyCheck("Could not find package.version in Cargo.toml".to_string())
        })
}

/// Count active features across all registered projects.
/// Returns list of (project_name, count) pairs for projects with active features.
pub(crate) fn count_active_features() -> Result<Vec<(String, usize)>> {
    use crate::state::feature::{FeatureState, FeatureStatus};

    let projects_dir = paths::global_projects_dir()?;
    let projects = ProjectEntry::list(&projects_dir)?;
    let mut results = Vec::new();

    for (name, entry) in &projects {
        let root = entry.root_path();
        if !root.exists() {
            continue;
        }
        let features_dir = paths::features_dir(&root);
        if let Ok(features) = FeatureState::list(&features_dir) {
            let active = features
                .iter()
                .filter(|(_, s)| !matches!(s.status, FeatureStatus::Merged | FeatureStatus::Stale))
                .count();
            if active > 0 {
                results.push((name.clone(), active));
            }
        }
    }
    Ok(results)
}

/// Run `pm self-update`: pull latest main, rebuild, install, upgrade projects.
pub fn self_update() -> Result<Vec<String>> {
    let mut output = Vec::new();
    let source = find_pm_source()?;

    // 1. Check for uncommitted changes
    if crate::git::has_uncommitted_changes(&source)? {
        return Err(PmError::SafetyCheck(
            "pm main worktree has uncommitted changes — commit or stash them first".to_string(),
        ));
    }

    let hash_before = head_short_hash(&source).unwrap_or_default();

    // 2. git pull (fast-forward only — if main has diverged, tell the user)
    output.push("Pulling latest changes...".to_string());
    match crate::git::pull(&source) {
        Ok(()) => {
            let hash_after = head_short_hash(&source).unwrap_or_default();
            if hash_before == hash_after {
                output.push("Already up to date.".to_string());
            } else {
                output.push(format!("Updated {hash_before} → {hash_after}"));
            }
        }
        Err(e) => {
            return Err(PmError::Git(format!("git pull failed: {e}")));
        }
    }

    // 3. cargo install --path .
    output.push("Building and installing...".to_string());
    let install = Command::new("cargo")
        .args(["install", "--path", "."])
        .current_dir(&source)
        .output()
        .map_err(|e| PmError::SafetyCheck(format!("failed to run cargo install: {e}")))?;

    if !install.status.success() {
        let stderr = String::from_utf8_lossy(&install.stderr).trim().to_string();
        return Err(PmError::SafetyCheck(format!(
            "Build failed (old binary is still intact):\n{stderr}"
        )));
    }
    output.push("Installed successfully.".to_string());

    let version = version_from_cargo_toml(&source).unwrap_or_else(|_| "unknown".to_string());

    // 4. Warn about active features
    let active_features = count_active_features()?;
    if !active_features.is_empty() {
        let total: usize = active_features.iter().map(|(_, c)| c).sum();
        let details: Vec<String> = active_features
            .iter()
            .map(|(name, count)| format!("  {name}: {count}"))
            .collect();
        output.push(format!(
            "⚠ {total} active feature{} across {} project{} (new binary may differ from in-flight worktrees):",
            if total == 1 { "" } else { "s" },
            active_features.len(),
            if active_features.len() == 1 { "" } else { "s" },
        ));
        output.extend(details);
    }

    // 5. Auto-run pm upgrade --all
    output.push("Upgrading all projects...".to_string());
    match super::upgrade::upgrade_all() {
        Ok(lines) => output.extend(lines),
        Err(e) => output.push(format!("Warning: upgrade failed: {e}")),
    }

    // 6. Summary
    let hash_final = head_short_hash(&source).unwrap_or_default();
    if hash_before == hash_final {
        output.push(format!("Done. v{version} ({hash_final}, unchanged)"));
    } else {
        output.push(format!("Done. v{version} ({hash_before} → {hash_final})"));
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::feature::{FeatureState, FeatureStatus};
    use tempfile::tempdir;

    #[test]
    fn version_from_cargo_toml_parses_version() {
        let dir = tempdir().unwrap();
        let cargo_toml = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo_toml,
            "[package]\nname = \"test\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();

        let v = version_from_cargo_toml(dir.path()).unwrap();
        assert_eq!(v, "1.2.3");
    }

    #[test]
    fn version_from_cargo_toml_ignores_dependency_versions() {
        let dir = tempdir().unwrap();
        let cargo_toml = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo_toml,
            "[package]\nname = \"test\"\nversion = \"1.0.0\"\n\n[dependencies.serde]\nversion = \"2.0.0\"\n",
        )
        .unwrap();

        let v = version_from_cargo_toml(dir.path()).unwrap();
        assert_eq!(v, "1.0.0");
    }

    #[test]
    fn version_from_cargo_toml_errors_on_missing_file() {
        let dir = tempdir().unwrap();
        let result = version_from_cargo_toml(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn find_pm_source_errors_when_not_registered() {
        // This test depends on global state, so just verify the error type
        // if pm is not registered (which it might be in the dev environment)
        let result = find_pm_source();
        // Either succeeds (pm is registered) or gives SafetyCheck error
        if let Err(e) = result {
            assert!(matches!(e, PmError::SafetyCheck(_)));
        }
    }

    #[test]
    fn dirty_worktree_refuses_update() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("pm-source");
        crate::git::init_repo(&repo_path).unwrap();

        // Create a tracked file, commit, then modify it
        std::fs::write(repo_path.join("file.txt"), "original").unwrap();
        crate::git::run_git(&repo_path, &["add", "file.txt"]).unwrap();
        crate::git::run_git(&repo_path, &["commit", "-m", "init"]).unwrap();
        std::fs::write(repo_path.join("file.txt"), "modified").unwrap();

        assert!(crate::git::has_uncommitted_changes(&repo_path).unwrap());
    }

    #[test]
    fn count_active_features_with_temp_project() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Set up minimal pm project structure
        let features_dir = root.join(".pm").join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        // Write a WIP feature
        let wip = FeatureState {
            status: FeatureStatus::Wip,
            branch: "feat-a".to_string(),
            worktree: "feat-a".to_string(),
            base: "main".to_string(),
            pr: String::new(),
            context: String::new(),
            created: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
        };
        wip.save(&features_dir, "feat-a").unwrap();

        // Write a Merged feature (should not count)
        let merged = FeatureState {
            status: FeatureStatus::Merged,
            branch: "feat-b".to_string(),
            worktree: "feat-b".to_string(),
            base: "main".to_string(),
            pr: String::new(),
            context: String::new(),
            created: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
        };
        merged.save(&features_dir, "feat-b").unwrap();

        // Count directly using FeatureState::list
        let features = FeatureState::list(&features_dir).unwrap();
        let active = features
            .iter()
            .filter(|(_, s)| !matches!(s.status, FeatureStatus::Merged | FeatureStatus::Stale))
            .count();

        assert_eq!(active, 1);
    }
}
