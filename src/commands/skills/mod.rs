//! Bundled assets and the tiered store they install into.
//!
//! Every bundled kind installs once into the **global tier**: skills, agent
//! definitions, and the baseline under `~/.agents/`, workflows under the pm
//! config dir. `pm init`/`pm upgrade` refresh it and the bundle is
//! authoritative there (bundled names are reserved). The **project tier**
//! (`main/.agents/{agents,skills}`, `.pm/workflows/`) holds only the user's
//! customs and shadows the global tier by name. No harness reads pm's
//! `.agents/agents/`, so each tier's store is *projected* into the harness's
//! own layout (`~/.claude/` and `main/.claude/` for claude-code) via
//! [`Harness::project_assets`]; the canonical copy always overwrites a
//! same-named projected file. Claude Code resolves a personal skill over a
//! project one, so a project custom of a global skill's name never applies
//! there: `pm doctor` reports it ([`shadowed_project_skills`]).
//!
//! Projection deletes only three things:
//! [`write_atomic`](crate::fs_utils::write_atomic) temp files whose writer
//! has exited, a feature's projection of a skill its branch deleted
//! ([`seed`](super::seed)), and an item the global `[bundled.disable]`
//! table disables ([`bundled_disable`](super::bundled_disable)), which
//! every global install removes from the global tier and its projections.

mod audit;
mod bundled;
mod global_store;
mod install;
mod migration;
mod projection;

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::harness::Harness;
use crate::state::paths;

use super::bundled_disable::Disabled;

pub use audit::{
    definition_files, disabled_still_installed, disabled_still_installed_in, global_store_missing,
    global_store_missing_in, redundant_overrides, shadowed_project_skills,
    shadowed_project_skills_in, unprojected_global_definitions, unprojected_global_definitions_in,
};
pub(crate) use bundled::{BundledKind, bundled_items, is_bundled};
pub use bundled::{bundled_workflow_names, is_bundled_workflow};
pub use global_store::{
    GlobalStore, install_global, install_global_dry_run, install_global_dry_run_in,
    install_global_in,
};
pub use migration::{
    is_migrated, migrate_project_to_global, stale_bundled_copies, write_migration_marker,
};
pub(crate) use migration::{scoped_worktrees_on_disk, worktrees_on_disk};
pub use projection::{global_customs_in, harnesses_in_use, project_assets};

use bundled::{is_installed, items_of_kind};
use global_store::{install_kind_global, uninstall_global};
use install::status_label;
use projection::project_dir;

/// The canonical asset store, relative to the main worktree or home.
pub const CANONICAL_DIR: &str = ".agents";

const BASELINE_FILE: &str = "pm-baseline.md";

/// One line per bundled item of `kind`: its global status (or that
/// `[bundled.disable]` lists it), and — inside a project — whether a same-named
/// project custom shadows it.
fn list_kind(kind: BundledKind, project_root: Option<&Path>) -> Result<Vec<String>> {
    let store = GlobalStore::resolve()?;
    let disabled = Disabled::load_in(&store.config_dir).unwrap_or_default();
    let global = store.dir(kind);
    let mut lines = Vec::new();
    for item in items_of_kind(kind) {
        let status = match (
            disabled.contains(kind, item.name),
            is_installed(&global, item),
        ) {
            (true, true) => "disabled (still installed; run `pm upgrade`)",
            (true, false) => "disabled",
            (false, _) => status_label(&global, item),
        };
        let mut line = format!("  {} — {status}", item.name);
        if project_root.is_some_and(|r| is_installed(&project_dir(r, kind), item)) {
            line.push_str(" (overridden by project custom)");
        }
        lines.push(line);
    }
    Ok(lines)
}

// --- Public API: Skills ---

pub fn skills_list(project_root: Option<&Path>) -> Result<Vec<String>> {
    list_kind(BundledKind::Skill, project_root)
}

pub fn skills_install(name: Option<&str>) -> Result<Vec<String>> {
    install_kind_global(BundledKind::Skill, name)
}

pub fn skills_uninstall(name: Option<&str>) -> Result<Vec<String>> {
    uninstall_global(BundledKind::Skill, name)
}

// --- Public API: Agents ---

pub fn agents_list(project_root: Option<&Path>) -> Result<Vec<String>> {
    list_kind(BundledKind::Agent, project_root)
}

pub fn agents_install(name: Option<&str>) -> Result<Vec<String>> {
    install_kind_global(BundledKind::Agent, name)
}

pub fn agents_uninstall(name: Option<&str>) -> Result<Vec<String>> {
    uninstall_global(BundledKind::Agent, name)
}

// --- Public API: Baseline ---

/// Absolute path of the shared baseline to apply for this project: the
/// global `~/.agents/pm-baseline.md`. A project spawning on a new binary
/// before its `pm upgrade` ran may only have the pre-migration project copy
/// (`main/.agents/`, or the older `main/.claude/`), so those are returned
/// when the global one is absent — unless `[bundled.disable]` lists the
/// baseline, when only the global path (gone after `pm upgrade`) is. The
/// caller checks existence either way.
pub fn baseline_path(project_root: &Path) -> PathBuf {
    baseline_path_in(project_root, GlobalStore::resolve().ok().as_ref())
}

pub fn baseline_path_in(project_root: &Path, store: Option<&GlobalStore>) -> PathBuf {
    let global = store.map(GlobalStore::baseline_path);
    if let Some(g) = &global
        && g.exists()
    {
        return g.clone();
    }
    if let Some(s) = store
        && Disabled::load_in(&s.config_dir).is_ok_and(|d| d.baseline())
    {
        return s.baseline_path();
    }
    let canonical = project_dir(project_root, BundledKind::Baseline).join(BASELINE_FILE);
    if canonical.exists() {
        return canonical;
    }
    let legacy = legacy_baseline_path(project_root);
    if legacy.exists() {
        return legacy;
    }
    global.unwrap_or(canonical)
}

/// Where releases before the canonical store installed the baseline.
fn legacy_baseline_path(project_root: &Path) -> PathBuf {
    paths::main_worktree(project_root)
        .join(Harness::ClaudeCode.config_dir())
        .join(BASELINE_FILE)
}

#[cfg(test)]
mod test_support {
    use std::fs;

    use super::GlobalStore;
    use super::bundled::{BundledItem, BundledKind, find_item};

    pub(super) fn item(kind: BundledKind, name: &str) -> &'static BundledItem {
        find_item(kind, name).unwrap()
    }

    pub(super) fn write_global_config(store: &GlobalStore, toml: &str) {
        fs::create_dir_all(&store.config_dir).unwrap();
        fs::write(store.config_dir.join("config.toml"), toml).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::skills::test_support::*;
    use std::fs;

    #[test]
    fn list_kind_reports_global_status_and_project_override() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        install_global().unwrap();
        let lines = agents_list(Some(project_root)).unwrap();
        assert!(
            lines.contains(&"  reviewer — installed".to_string()),
            "{lines:?}"
        );

        let custom = paths::main_worktree(project_root).join(".agents/agents/reviewer.md");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        fs::write(&custom, "mine").unwrap();
        let lines = agents_list(Some(project_root)).unwrap();
        assert!(
            lines.contains(&"  reviewer — installed (overridden by project custom)".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"  implementer — installed".to_string()),
            "{lines:?}"
        );
        assert!(
            agents_list(None)
                .unwrap()
                .iter()
                .all(|l| !l.contains("overridden"))
        );
    }

    // --- Baseline ---

    #[test]
    fn baseline_path_prefers_global_then_project_then_legacy() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let main = paths::main_worktree(project_root);
        let canonical = main.join(".agents/pm-baseline.md");
        let legacy = main.join(".claude/pm-baseline.md");
        let global = store.baseline_path();

        // Nothing installed: the global path, for callers to test.
        assert_eq!(baseline_path_in(project_root, Some(&store)), global);
        assert!(!global.exists());

        for (path, content) in [
            (&legacy, "legacy"),
            (&canonical, "project"),
            (&global, "global"),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
            assert_eq!(&baseline_path_in(project_root, Some(&store)), path);
        }
        // No resolvable home: the project chain still applies.
        assert_eq!(baseline_path_in(project_root, None), canonical);
    }

    #[test]
    fn a_disabled_baseline_never_falls_back_to_a_project_copy() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let main = paths::main_worktree(tmp.path());
        for copy in [".agents/pm-baseline.md", ".claude/pm-baseline.md"] {
            fs::create_dir_all(main.join(copy).parent().unwrap()).unwrap();
            fs::write(main.join(copy), "stale").unwrap();
        }
        write_global_config(&store, "[bundled.disable]\nbaseline = true\n");
        assert_eq!(
            baseline_path_in(tmp.path(), Some(&store)),
            store.baseline_path()
        );
    }
}
