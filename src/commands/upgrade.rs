use std::path::Path;

use crate::error::Result;
use crate::state::paths;
use crate::state::project::ProjectEntry;

use super::claude_settings;
use super::hooks_install;
use super::skills;

/// Upgrade a single project: reinstall hooks, skills, and agents to main,
/// then re-seed `.claude/` settings into each active feature worktree.
/// Returns a human-readable summary line.
pub fn upgrade_project(project_root: &Path) -> Result<String> {
    let mut updated = Vec::new();

    // Install hooks
    let _ = hooks_install::install(project_root)?;
    updated.push("hooks");

    // Bootstrap information store and state repo (both idempotent)
    super::docs::bootstrap(project_root)?;
    super::state_cmd::init(project_root)?;
    // Migrate docs submodule to regular files if needed
    if super::docs::migrate_docs_submodule(project_root).unwrap_or(false) {
        updated.push("docs (migrated from submodule)");
    } else {
        updated.push("docs");
    }

    // Install skills to main
    let _ = skills::skills_install_project(project_root, None)?;
    updated.push("skills");

    // Install agents to main
    let _ = skills::agents_install_project(project_root, None)?;
    updated.push("agents");

    // Re-seed each active feature worktree
    let features_dir = paths::features_dir(project_root);
    let features = crate::state::feature::FeatureState::list(&features_dir)?;
    let mut feature_count = 0;
    for (name, _state) in &features {
        let feature_worktree = project_root.join(name);
        if feature_worktree.is_dir() {
            claude_settings::seed_feature_claude(project_root, &feature_worktree)?;
            feature_count += 1;
        }
    }

    let parts = updated.join(", ");
    if feature_count > 0 {
        Ok(format!(
            "Upgraded {parts} for main + {feature_count} feature{}",
            if feature_count == 1 { "" } else { "s" }
        ))
    } else {
        Ok(format!("Upgraded {parts} for main"))
    }
}

/// Upgrade all registered projects.
/// Returns one summary line per project.
pub fn upgrade_all() -> Result<Vec<String>> {
    let projects_dir = paths::global_projects_dir()?;
    let projects = ProjectEntry::list(&projects_dir)?;

    if projects.is_empty() {
        return Ok(vec!["No registered projects".to_string()]);
    }

    let mut lines = Vec::new();
    for (name, entry) in &projects {
        // Migrate absolute paths to portable ~/… format on re-save
        let portable = crate::path_utils::to_portable(&entry.root_path());
        if portable != entry.root {
            let migrated = ProjectEntry {
                root: portable,
                ..entry.clone()
            };
            migrated.save(&projects_dir, name)?;
        }

        let root = entry.root_path();
        if !root.exists() {
            lines.push(format!("{name}: skipped (root does not exist)"));
            continue;
        }
        match upgrade_project(&root) {
            Ok(summary) => lines.push(format!("{name}: {summary}")),
            Err(e) => lines.push(format!("{name}: error: {e}")),
        }
    }
    Ok(lines)
}

/// Upgrade either the current project (default) or all projects (--all).
pub fn upgrade(all: bool) -> Result<Vec<String>> {
    if all {
        upgrade_all()
    } else {
        let project_root = paths::find_project_root(&std::env::current_dir()?)?;
        let line = upgrade_project(&project_root)?;
        Ok(vec![line])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn setup_project(dir: &std::path::Path) -> PathBuf {
        let root = dir.to_path_buf();
        fs::create_dir_all(root.join(".pm").join("features")).unwrap();
        fs::create_dir_all(paths::main_worktree(&root)).unwrap();
        root
    }

    fn write_feature_toml(root: &std::path::Path, name: &str) {
        let content = format!(
            r#"status = "wip"
branch = "{name}"
worktree = "{name}"
base = ""
pr = ""
context = ""
created = "2026-01-01T00:00:00Z"
last_active = "2026-01-01T00:00:00Z"
"#
        );
        fs::write(
            root.join(".pm")
                .join("features")
                .join(format!("{name}.toml")),
            content,
        )
        .unwrap();
    }

    #[test]
    fn upgrade_installs_hooks_skills_agents() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let summary = upgrade_project(&root).unwrap();
        assert!(summary.contains("hooks"));
        assert!(summary.contains("skills"));
        assert!(summary.contains("agents"));
        assert!(summary.contains("for main"));

        // Verify hooks installed
        assert!(hooks_install::is_installed(&root).unwrap());

        // Verify skills installed
        let skill_path = paths::main_worktree(&root)
            .join(".claude")
            .join("skills")
            .join("pm")
            .join("SKILL.md");
        assert!(skill_path.exists());

        // Verify agents installed
        let agent_path = paths::main_worktree(&root)
            .join(".claude")
            .join("agents")
            .join("reviewer.md");
        assert!(agent_path.exists());
    }

    #[test]
    fn upgrade_reseeds_feature_worktrees() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Create a feature with state and worktree dir
        write_feature_toml(&root, "my-feat");
        fs::create_dir_all(root.join("my-feat")).unwrap();

        let summary = upgrade_project(&root).unwrap();
        assert!(summary.contains("1 feature"));

        // The feature should have settings seeded from main
        let feat_settings = root.join("my-feat").join(".claude").join("settings.json");
        // settings.json is only copied if it exists in main, which it does
        // after hooks install
        assert!(feat_settings.exists());
    }

    #[test]
    fn upgrade_reports_multiple_features() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        for name in &["feat-a", "feat-b", "feat-c"] {
            write_feature_toml(&root, name);
            fs::create_dir_all(root.join(name)).unwrap();
        }

        let summary = upgrade_project(&root).unwrap();
        assert!(summary.contains("3 features"));
    }

    #[test]
    fn upgrade_skips_features_without_worktree_dir() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Feature state exists but no worktree directory
        write_feature_toml(&root, "orphan");

        let summary = upgrade_project(&root).unwrap();
        assert!(summary.contains("for main"));
        assert!(!summary.contains("feature"));
    }

    #[test]
    fn upgrade_is_idempotent() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let first = upgrade_project(&root).unwrap();
        let second = upgrade_project(&root).unwrap();

        // Both should succeed
        assert!(first.contains("Upgraded"));
        assert!(second.contains("Upgraded"));
    }

    #[test]
    fn upgrade_bootstraps_docs() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let summary = upgrade_project(&root).unwrap();
        assert!(summary.contains("docs"));

        let docs_dir = root.join(".pm").join("docs");
        assert!(docs_dir.join("categories.toml").exists());
        assert!(docs_dir.join("todo.md").exists());
        // Docs are tracked by the parent .pm/ state repo, not a separate git repo
        assert!(!docs_dir.join(".git").exists());
        assert!(root.join(".pm").join(".git").exists());
    }
}
