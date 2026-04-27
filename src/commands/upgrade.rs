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

/// Dry-run variant of [`upgrade_project`]: report what would change without
/// writing anything. Returns one `Would …` line per action that would be
/// taken; an empty `Vec` means the project is fully up to date. The public
/// [`upgrade`] dispatcher is responsible for translating an empty result
/// into the user-facing `Up to date` line.
pub fn upgrade_project_dry_run(project_root: &Path) -> Result<Vec<String>> {
    let mut actions = Vec::new();

    // Hooks
    if let Some(line) = hooks_install::install_dry_run(project_root)? {
        actions.push(line);
    }

    // Information store (.pm/docs/) bootstrap
    for path in super::docs::bootstrap_dry_run(project_root) {
        actions.push(format!(
            "Would create {}",
            display_path(project_root, &path)
        ));
    }

    // State repo (.pm/) init
    if super::state_cmd::would_init(project_root) {
        let pm_dir = paths::pm_dir(project_root);
        actions.push(format!(
            "Would initialise state repo in {}",
            display_path(project_root, &pm_dir)
        ));
    }

    // Submodule migration
    if super::docs::would_migrate_docs_submodule(project_root) {
        actions.push("Would migrate .pm/docs/ from submodule to regular files".to_string());
    }

    // Skills (each line is already an action — no filtering needed)
    actions.extend(skills::skills_install_project_dry_run(project_root, None)?);

    // Agents
    actions.extend(skills::agents_install_project_dry_run(project_root, None)?);

    // Feature worktrees: only report each feature whose `.claude/` differs
    let features_dir = paths::features_dir(project_root);
    let features = crate::state::feature::FeatureState::list(&features_dir)?;
    for (name, _state) in &features {
        let feature_worktree = project_root.join(name);
        if !feature_worktree.is_dir() {
            continue;
        }
        if claude_settings::seed_feature_claude_would_change(project_root, &feature_worktree)? {
            actions.push(format!("Would re-seed .claude/ in feature '{name}'"));
        }
    }

    Ok(actions)
}

/// Wraps [`upgrade_project_dry_run`] with the `Up to date` fallback used by
/// the public dispatcher. Lifted out of [`upgrade`] so it can be tested
/// without mutating the process-wide cwd.
fn upgrade_dry_run_at(project_root: &Path) -> Result<Vec<String>> {
    let actions = upgrade_project_dry_run(project_root)?;
    if actions.is_empty() {
        Ok(vec!["Up to date".to_string()])
    } else {
        Ok(actions)
    }
}

/// Format a path relative to `project_root` when possible, otherwise display
/// the full path. Keeps dry-run output concise and stable across machines.
fn display_path(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
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

/// Dry-run variant of [`upgrade_all`]: preview what would change for every
/// registered project without writing anything.
pub fn upgrade_all_dry_run() -> Result<Vec<String>> {
    let projects_dir = paths::global_projects_dir()?;
    let projects = ProjectEntry::list(&projects_dir)?;

    if projects.is_empty() {
        return Ok(vec!["No registered projects".to_string()]);
    }

    let mut lines = Vec::new();
    for (name, entry) in &projects {
        let root = entry.root_path();
        if !root.exists() {
            lines.push(format!("{name}: skipped (root does not exist)"));
            continue;
        }
        match upgrade_project_dry_run(&root) {
            Ok(actions) if actions.is_empty() => {
                lines.push(format!("{name}: up to date"));
            }
            Ok(actions) => {
                lines.push(format!("{name}:"));
                for action in actions {
                    lines.push(format!("  {action}"));
                }
            }
            Err(e) => lines.push(format!("{name}: error: {e}")),
        }
    }
    Ok(lines)
}

/// Upgrade either the current project (default) or all projects (--all).
/// When `dry_run` is `true`, preview changes without writing anything.
pub fn upgrade(all: bool, dry_run: bool) -> Result<Vec<String>> {
    if all {
        if dry_run {
            upgrade_all_dry_run()
        } else {
            upgrade_all()
        }
    } else {
        let project_root = paths::find_project_root(&std::env::current_dir()?)?;
        if dry_run {
            upgrade_dry_run_at(&project_root)
        } else {
            let line = upgrade_project(&project_root)?;
            Ok(vec![line])
        }
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

    // --- Dry-run tests ---

    #[test]
    fn dry_run_reports_actions_on_fresh_project() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let actions = upgrade_project_dry_run(&root).unwrap();
        assert!(!actions.is_empty(), "expected actions on fresh project");

        let joined = actions.join("\n");
        assert!(
            joined.contains("Would install pm hooks"),
            "missing hooks line, got: {joined}"
        );
        assert!(
            joined.contains("Would install Skill 'pm'"),
            "missing pm skill line, got: {joined}"
        );
        assert!(
            joined.contains("Would install Agent 'reviewer'"),
            "missing reviewer agent line, got: {joined}"
        );
        assert!(
            joined.contains("Would create"),
            "missing docs create lines, got: {joined}"
        );
        assert!(
            joined.contains("Would initialise state repo"),
            "missing state repo line, got: {joined}"
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let _ = upgrade_project_dry_run(&root).unwrap();

        // Ensure none of the side effects of a real upgrade landed
        let settings_path = paths::main_worktree(&root)
            .join(".claude")
            .join("settings.json");
        assert!(
            !settings_path.exists(),
            "settings.json should not be written"
        );
        assert!(
            !root.join(".pm").join("docs").exists(),
            "docs/ should not be created"
        );
        assert!(
            !root.join(".pm").join(".git").exists(),
            "state repo should not be initialised"
        );
        let skill_path = paths::main_worktree(&root)
            .join(".claude")
            .join("skills")
            .join("pm")
            .join("SKILL.md");
        assert!(!skill_path.exists(), "skill should not be installed");
    }

    #[test]
    fn dry_run_returns_empty_when_up_to_date() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Apply a real upgrade first
        upgrade_project(&root).unwrap();

        let actions = upgrade_project_dry_run(&root).unwrap();
        assert!(
            actions.is_empty(),
            "expected no actions after upgrade, got: {actions:?}"
        );
    }

    #[test]
    fn dry_run_reports_outdated_skill() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        upgrade_project(&root).unwrap();

        // Corrupt an installed skill so it's no longer up to date
        let skill_path = paths::main_worktree(&root)
            .join(".claude")
            .join("skills")
            .join("pm")
            .join("SKILL.md");
        fs::write(&skill_path, "stale content").unwrap();

        let actions = upgrade_project_dry_run(&root).unwrap();
        let joined = actions.join("\n");
        assert!(
            joined.contains("Would update Skill 'pm'"),
            "expected update line, got: {joined}"
        );
    }

    #[test]
    fn dry_run_reports_feature_reseed_when_stale() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        upgrade_project(&root).unwrap();

        // Add a feature with stale .claude/ contents
        write_feature_toml(&root, "stale-feat");
        let feat_dir = root.join("stale-feat");
        let feat_claude = feat_dir.join(".claude");
        fs::create_dir_all(&feat_claude).unwrap();
        fs::write(feat_claude.join("settings.json"), "{}").unwrap();

        let actions = upgrade_project_dry_run(&root).unwrap();
        let joined = actions.join("\n");
        assert!(
            joined.contains("Would re-seed .claude/ in feature 'stale-feat'"),
            "expected feature line, got: {joined}"
        );
    }

    #[test]
    fn dry_run_at_returns_up_to_date_when_no_actions() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        upgrade_project(&root).unwrap();

        let lines = upgrade_dry_run_at(&root).unwrap();
        assert_eq!(lines, vec!["Up to date".to_string()]);
    }

    #[test]
    fn dry_run_at_returns_actions_when_changes_pending() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let lines = upgrade_dry_run_at(&root).unwrap();
        assert!(
            lines.iter().all(|l| l != "Up to date"),
            "expected actions, not the up-to-date sentinel: {lines:?}"
        );
        assert!(!lines.is_empty(), "expected at least one action line");
    }
}
