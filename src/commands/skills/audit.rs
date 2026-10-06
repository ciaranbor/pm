//! Read-only checks over the tiers that `pm doctor` reports.

use std::fs;
use std::path::Path;

use crate::error::Result;
use crate::harness::Harness;

use super::super::bundled_disable::Disabled;
use super::GlobalStore;
use super::bundled::{BundledKind, is_installed, is_up_to_date, items_of_kind};
use super::global_store::remove_disabled;
use super::install::enabled_items;
use super::projection::{harnesses_in_use, project_dir};

/// Items `[bundled.disable]` lists that still have a copy in the global tier or
/// a harness's global projection (as `Kind 'name'`): what the next install
/// removes.
/// A config that doesn't parse disables nothing here, so `pm doctor` keeps
/// running to report it.
pub fn disabled_still_installed() -> Result<Vec<String>> {
    disabled_still_installed_in(&GlobalStore::resolve()?)
}

pub fn disabled_still_installed_in(store: &GlobalStore) -> Result<Vec<String>> {
    let disabled = Disabled::load_in(&store.config_dir).unwrap_or_default();
    remove_disabled(store, &disabled, true)
}

/// Bundled items (as `Kind 'name'`) absent from the global tier.
pub fn global_store_missing() -> Result<Vec<String>> {
    Ok(global_store_missing_in(&GlobalStore::resolve()?))
}

pub fn global_store_missing_in(store: &GlobalStore) -> Vec<String> {
    let disabled = Disabled::load_in(&store.config_dir).unwrap_or_default();
    let mut out = Vec::new();
    for kind in BundledKind::ALL {
        let dir = store.dir(kind);
        for item in enabled_items(kind, &disabled) {
            if !is_installed(&dir, item) {
                out.push(format!("{} '{}'", kind.label(), item.name));
            }
        }
    }
    out
}

/// Global agent definitions (`~/.agents/agents/*.md`) with no projected copy
/// in `harnesses`' global definition dirs. Callers pass the harnesses whose
/// projections matter to them — a project passes the ones it uses, so an
/// unused harness never raises a finding.
pub fn unprojected_global_definitions(harnesses: &[Harness]) -> Result<Vec<(String, Harness)>> {
    unprojected_global_definitions_in(&GlobalStore::resolve()?, harnesses)
}

pub fn unprojected_global_definitions_in(
    store: &GlobalStore,
    harnesses: &[Harness],
) -> Result<Vec<(String, Harness)>> {
    let canonical = store.dir(BundledKind::Agent);
    let mut out = Vec::new();
    for file in definition_files(&canonical)? {
        for h in harnesses.iter().filter(|h| h.projects_definitions()) {
            let projected = h
                .global_config_dir(&store.home)
                .map(|d| d.join("agents").join(&file));
            if projected.is_some_and(|p| !p.exists()) {
                out.push((file.trim_end_matches(".md").to_string(), *h));
            }
        }
    }
    Ok(out)
}

/// Sorted `*.md` filenames directly under `dir`; empty when it doesn't exist.
pub fn definition_files(dir: &Path) -> Result<Vec<String>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|f| f.ends_with(".md"))
        .collect();
    names.sort();
    Ok(names)
}

/// Project custom skills (`main/.agents/skills/<name>`) that a harness
/// resolves the *global* same-named skill over, so the custom never takes
/// effect. Claude Code's personal-over-project precedence is the one place
/// project-shadows-global can't be delivered by placement.
pub fn shadowed_project_skills(project_root: &Path) -> Result<Vec<(String, Harness)>> {
    shadowed_project_skills_in(project_root, &GlobalStore::resolve()?)
}

pub fn shadowed_project_skills_in(
    project_root: &Path,
    store: &GlobalStore,
) -> Result<Vec<(String, Harness)>> {
    let dir = project_dir(project_root, BundledKind::Skill);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let harnesses = harnesses_in_use(project_root)?;
    let mut names: Vec<String> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        for h in &harnesses {
            if h.project_skill_shadowed_by_global(&store.home, &name) {
                out.push((name.clone(), *h));
            }
        }
    }
    Ok(out)
}

/// Project-tier files with a bundled name whose content equals the current
/// bundle (as `Kind 'name'`): an override that changes nothing.
pub fn redundant_overrides(project_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for kind in [
        BundledKind::Skill,
        BundledKind::Agent,
        BundledKind::Workflow,
    ] {
        let dir = project_dir(project_root, kind);
        for item in items_of_kind(kind) {
            if is_up_to_date(&dir, item) {
                out.push(format!("{} '{}'", kind.label(), item.name));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::skills::install::install_in;
    use crate::commands::skills::install_global_in;
    use crate::state::paths;

    #[test]
    fn redundant_override_is_a_bundled_named_file_with_bundled_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let agents = paths::main_worktree(project_root).join(".agents/agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("reviewer.md"), "my reviewer").unwrap();
        assert!(redundant_overrides(project_root).is_empty());

        install_in(&agents, BundledKind::Agent, Some("reviewer")).unwrap();
        install_in(
            &paths::workflows_dir(project_root),
            BundledKind::Workflow,
            Some("solo"),
        )
        .unwrap();
        assert_eq!(
            redundant_overrides(project_root),
            vec![
                "Agent 'reviewer'".to_string(),
                "Workflow 'solo'".to_string()
            ]
        );
    }

    #[test]
    fn project_skill_is_shadowed_when_the_personal_dir_has_its_name() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        install_global_in(&store).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let skills = paths::main_worktree(project_root).join(".agents/skills");
        for name in ["pm", "mine"] {
            fs::create_dir_all(skills.join(name)).unwrap();
            fs::write(skills.join(name).join("SKILL.md"), "custom").unwrap();
        }
        assert_eq!(
            shadowed_project_skills_in(project_root, &store).unwrap(),
            vec![("pm".to_string(), Harness::ClaudeCode)]
        );
    }
}
