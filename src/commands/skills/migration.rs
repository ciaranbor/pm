//! Migration: a project's bundled copies give way to the global tier.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::fs_utils::write_atomic;
use crate::state::paths;

use super::bundled::{BundledKind, items_of_kind};
use super::install::prune_empty_parents;
use super::projection::{harnesses_in_use, project_dir};
use super::{BASELINE_FILE, CANONICAL_DIR, legacy_baseline_path};

/// Marker file under `.pm/migrations/` recording that the project's bundled
/// copies were removed in favour of the global tier.
const MIGRATION_MARKER: &str = "global-assets";

/// Whether this project's bundled copies have been migrated to the global
/// tier. Until then, bundled-named project files are stale pm-owned copies;
/// after, they are the user's customs.
pub fn is_migrated(project_root: &Path) -> bool {
    paths::migration_marker(project_root, MIGRATION_MARKER).exists()
}

pub fn write_migration_marker(project_root: &Path) -> Result<()> {
    write_atomic(
        &paths::migration_marker(project_root, MIGRATION_MARKER),
        b"",
    )
}

/// The project-tier paths a migration removes: every bundled skill/agent
/// file in main's and each feature worktree's canonical store and harness
/// projections, the baseline (canonical and legacy), and the bundled
/// workflow directories. Only what exists on disk.
pub fn stale_bundled_copies(project_root: &Path) -> Result<Vec<PathBuf>> {
    let mut stores = vec![PathBuf::from(CANONICAL_DIR)];
    stores.extend(
        harnesses_in_use(project_root)?
            .into_iter()
            .map(|h| PathBuf::from(h.config_dir())),
    );
    let mut out = Vec::new();
    for base in worktrees_on_disk(project_root)? {
        for store in &stores {
            for kind in [BundledKind::Skill, BundledKind::Agent] {
                let dir = base.join(store).join(kind.store_subdir().unwrap());
                for item in items_of_kind(kind) {
                    for (rel, _) in item.files {
                        let path = dir.join(rel);
                        if path.exists() {
                            out.push(path);
                        }
                    }
                }
            }
        }
    }
    for path in [
        project_dir(project_root, BundledKind::Baseline).join(BASELINE_FILE),
        legacy_baseline_path(project_root),
    ] {
        if path.exists() {
            out.push(path);
        }
    }
    let workflows = paths::workflows_dir(project_root);
    for item in items_of_kind(BundledKind::Workflow) {
        let dir = workflows.join(item.name);
        if dir.is_dir() {
            out.push(dir);
        }
    }
    Ok(out)
}

/// Main plus every feature worktree that exists on disk.
pub(crate) fn worktrees_on_disk(project_root: &Path) -> Result<Vec<PathBuf>> {
    Ok(scoped_worktrees_on_disk(project_root)?
        .into_iter()
        .map(|(_, wt)| wt)
        .collect())
}

/// [`worktrees_on_disk`] with each worktree's scope name.
pub(crate) fn scoped_worktrees_on_disk(project_root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = vec![("main".to_string(), paths::main_worktree(project_root))];
    let features_dir = paths::features_dir(project_root);
    if features_dir.is_dir() {
        for (name, _) in crate::state::feature::FeatureState::list(&features_dir)? {
            let wt = project_root.join(&name);
            if wt.is_dir() {
                out.push((name, wt));
            }
        }
    }
    Ok(out)
}

/// Remove the project's bundled copies (see [`stale_bundled_copies`]) and
/// write the migration marker. Returns the removed paths relative to the
/// project root; with `dry_run` nothing is written and the same list says
/// what would go.
pub fn migrate_project_to_global(project_root: &Path, dry_run: bool) -> Result<Vec<PathBuf>> {
    let paths = stale_bundled_copies(project_root)?;
    if dry_run {
        return Ok(paths.iter().map(|p| relative(project_root, p)).collect());
    }
    for path in &paths {
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
            // Stop at the store root (`.agents/` or `.claude/`): only the
            // bundled subtree is pm's.
            let stop = store_root(project_root, path);
            prune_empty_parents(path, &stop);
        }
    }
    for base in worktrees_on_disk(project_root)? {
        let canonical = base.join(CANONICAL_DIR);
        if canonical.read_dir().is_ok_and(|mut d| d.next().is_none()) {
            let _ = fs::remove_dir(&canonical);
        }
    }
    write_migration_marker(project_root)?;
    Ok(paths.iter().map(|p| relative(project_root, p)).collect())
}

/// `<worktree>/<store>` for a path inside a worktree's canonical store or
/// harness dir — the ancestor two levels below the project root.
fn store_root(project_root: &Path, path: &Path) -> PathBuf {
    let rel = path.strip_prefix(project_root).unwrap_or(path);
    let mut root = project_root.to_path_buf();
    for c in rel.components().take(2) {
        root.push(c);
    }
    root
}

fn relative(project_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(project_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::skills::install::install_in;

    fn project_with_feature(project_root: &Path) {
        let features_dir = paths::features_dir(project_root);
        fs::create_dir_all(&features_dir).unwrap();
        fs::write(
            features_dir.join("my-feat.toml"),
            "status = \"wip\"\nbranch = \"my-feat\"\nworktree = \"my-feat\"\nbase = \"main\"\n\
             pr = \"\"\ncontext = \"\"\ncreated = \"2026-01-01T00:00:00Z\"\n\
             last_active = \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();
        fs::create_dir_all(project_root.join("my-feat")).unwrap();
    }

    /// A pre-migration project: bundled copies in main's canonical store and
    /// harness dir, a seeded feature, both baselines, bundled and custom
    /// workflows, and customs beside the bundled files.
    fn pre_migration_project(project_root: &Path) {
        project_with_feature(project_root);
        for base in [
            paths::main_worktree(project_root),
            project_root.join("my-feat"),
        ] {
            for store in [".agents", ".claude"] {
                let dir = base.join(store);
                install_in(&dir.join("agents"), BundledKind::Agent, None).unwrap();
                install_in(&dir.join("skills"), BundledKind::Skill, None).unwrap();
                fs::write(dir.join("agents/custom.md"), "custom def").unwrap();
                fs::create_dir_all(dir.join("skills/mine")).unwrap();
                fs::write(dir.join("skills/mine/SKILL.md"), "custom skill").unwrap();
            }
        }
        let main = paths::main_worktree(project_root);
        fs::write(main.join(".agents/pm-baseline.md"), "baseline").unwrap();
        fs::write(main.join(".claude/pm-baseline.md"), "legacy baseline").unwrap();
        fs::write(main.join(".claude/settings.json"), "{}").unwrap();
        let workflows = paths::workflows_dir(project_root);
        install_in(&workflows, BundledKind::Workflow, None).unwrap();
        fs::write(
            workflows.join("solo/config.toml"),
            "description = \"old\"\nagents = [\"claude\"]\n",
        )
        .unwrap();
        fs::create_dir_all(workflows.join("my-flow")).unwrap();
        fs::write(
            workflows.join("my-flow/config.toml"),
            "description = \"mine\"\n",
        )
        .unwrap();
    }

    #[test]
    fn migration_removes_bundled_copies_everywhere_and_keeps_customs() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        pre_migration_project(project_root);
        let main = paths::main_worktree(project_root);
        let feat = project_root.join("my-feat");
        assert!(!is_migrated(project_root));

        let dry = migrate_project_to_global(project_root, true).unwrap();
        assert!(
            dry.contains(&PathBuf::from("main/.claude/agents/reviewer.md")),
            "{dry:?}"
        );
        assert!(
            dry.contains(&PathBuf::from("my-feat/.agents/skills/pm/SKILL.md")),
            "{dry:?}"
        );
        assert!(
            dry.contains(&PathBuf::from("main/.claude/pm-baseline.md")),
            "{dry:?}"
        );
        assert!(
            dry.contains(&PathBuf::from(".pm/workflows/solo")),
            "{dry:?}"
        );
        assert!(
            !dry.iter().any(|p| p.to_string_lossy().contains("custom")
                || p.to_string_lossy().contains("mine")
                || p.to_string_lossy().contains("my-flow")),
            "{dry:?}"
        );
        assert!(main.join(".claude/agents/reviewer.md").exists());
        assert!(!is_migrated(project_root));

        let removed = migrate_project_to_global(project_root, false).unwrap();
        assert_eq!(removed, dry);
        assert!(is_migrated(project_root));

        for base in [&main, &feat] {
            for store in [".agents", ".claude"] {
                let dir = base.join(store);
                for item in items_of_kind(BundledKind::Agent) {
                    assert!(
                        !dir.join("agents").join(item.files[0].0).exists(),
                        "{}",
                        dir.display()
                    );
                }
                assert!(!dir.join("skills/pm").exists(), "{}", dir.display());
                assert_eq!(
                    fs::read_to_string(dir.join("agents/custom.md")).unwrap(),
                    "custom def"
                );
                assert_eq!(
                    fs::read_to_string(dir.join("skills/mine/SKILL.md")).unwrap(),
                    "custom skill"
                );
            }
        }
        assert!(!main.join(".agents/pm-baseline.md").exists());
        assert!(!main.join(".claude/pm-baseline.md").exists());
        assert_eq!(
            fs::read_to_string(main.join(".claude/settings.json")).unwrap(),
            "{}"
        );
        let workflows = paths::workflows_dir(project_root);
        for item in items_of_kind(BundledKind::Workflow) {
            assert!(!workflows.join(item.name).exists(), "{}", item.name);
        }
        assert!(workflows.join("my-flow/config.toml").exists());

        // Nothing bundled-named remains, so a second pass is empty …
        assert!(
            migrate_project_to_global(project_root, true)
                .unwrap()
                .is_empty()
        );
        // … and after the marker, a bundled-named file is a custom override.
        fs::write(main.join(".agents/agents/reviewer.md"), "my reviewer").unwrap();
        assert!(stale_bundled_copies(project_root).unwrap().len() == 1);
        assert!(is_migrated(project_root));
    }

    #[test]
    fn migration_prunes_empty_stores_but_never_the_harness_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let main = paths::main_worktree(project_root);
        fs::create_dir_all(&main).unwrap();
        install_in(&main.join(".agents/agents"), BundledKind::Agent, None).unwrap();
        install_in(&main.join(".agents/skills"), BundledKind::Skill, None).unwrap();
        install_in(&main.join(".claude/agents"), BundledKind::Agent, None).unwrap();
        fs::write(main.join(".claude/settings.json"), "{}").unwrap();

        migrate_project_to_global(project_root, false).unwrap();
        assert!(!main.join(".agents").exists());
        assert!(!main.join(".claude/agents").exists());
        assert!(main.join(".claude/settings.json").exists());
    }

    #[test]
    fn restored_state_without_stores_still_migrates_bundled_workflows() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();
        let solo = paths::workflows_dir(project_root).join("solo");
        fs::create_dir_all(&solo).unwrap();
        fs::write(
            solo.join("config.toml"),
            "description = \"old\"\nagents = [\"claude\"]\n",
        )
        .unwrap();

        let removed = migrate_project_to_global(project_root, false).unwrap();
        assert_eq!(removed, vec![PathBuf::from(".pm/workflows/solo")]);
        assert!(!solo.exists());
        assert!(is_migrated(project_root));
    }
}
