//! The global tier: installing the bundle into it, removing what
//! `[bundled.disable]` lists, and projecting it into each harness's global dir.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::fs_utils::copy_dir_recursive;
use crate::harness::Harness;
use crate::state::paths;

use super::super::bundled_disable::Disabled;
use super::super::state_gitignore;
use super::bundled::{BUNDLED_ITEMS, BundledKind, items_of_kind};
use super::install::{
    Applied, enabled_items, install_items, install_items_dry_run, install_messages,
    prune_empty_parents, uninstall_in,
};
use super::projection::{project_global, project_global_from};
use super::{BASELINE_FILE, CANONICAL_DIR};

/// The global tier's on-disk locations. Production code resolves it from
/// the process environment; tests that mutate the tier build one over a
/// private tempdir with [`GlobalStore::at`].
pub struct GlobalStore {
    pub home: PathBuf,
    pub config_dir: PathBuf,
}

impl GlobalStore {
    pub fn resolve() -> Result<Self> {
        Ok(Self {
            home: paths::home_dir()?,
            config_dir: paths::global_config_dir()?,
        })
    }

    /// A store rooted entirely under `home`, with the config dir at
    /// `<home>/.config/pm` (also the layout `cfg(test)` resolution uses).
    pub fn at(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            config_dir: home.join(".config").join("pm"),
        }
    }

    pub(super) fn canonical(&self) -> PathBuf {
        self.home.join(CANONICAL_DIR)
    }

    pub fn workflows_dir(&self) -> PathBuf {
        paths::global_workflows_dir_in(&self.config_dir)
    }

    pub fn baseline_path(&self) -> PathBuf {
        self.canonical().join(BASELINE_FILE)
    }

    pub(super) fn dir(&self, kind: BundledKind) -> PathBuf {
        match kind {
            BundledKind::Skill | BundledKind::Agent => {
                self.canonical().join(kind.store_subdir().unwrap())
            }
            BundledKind::Baseline => self.canonical(),
            BundledKind::Workflow => self.workflows_dir(),
        }
    }
}

/// Where `kind` is projected in each supported harness's global dir; empty
/// for kinds that aren't projected.
fn global_projection_dirs(store: &GlobalStore, kind: BundledKind) -> Vec<PathBuf> {
    let Some(subdir) = kind.store_subdir() else {
        return Vec::new();
    };
    Harness::SUPPORTED
        .iter()
        .filter_map(|h| h.global_config_dir(&store.home))
        .map(|dir| dir.join(subdir))
        .collect()
}

/// Remove every item `disabled` names from the global tier and from each
/// supported harness's global projection, returning (as `Kind 'name'`) each
/// item that had a copy anywhere; `dry_run` deletes nothing. Only the
/// bundle's own file paths are removed.
pub(super) fn remove_disabled(
    store: &GlobalStore,
    disabled: &Disabled,
    dry_run: bool,
) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for item in BUNDLED_ITEMS
        .iter()
        .filter(|i| disabled.contains(i.kind, i.name))
    {
        let mut roots = vec![store.dir(item.kind)];
        roots.extend(global_projection_dirs(store, item.kind));
        let present: Vec<(PathBuf, &PathBuf)> = roots
            .iter()
            .flat_map(|root| {
                item.files
                    .iter()
                    .map(move |(rel, _)| (root.join(rel), root))
            })
            .filter(|(path, _)| path.exists())
            .collect();
        if present.is_empty() {
            continue;
        }
        if !dry_run {
            for (path, root) in &present {
                fs::remove_file(path)?;
                prune_empty_parents(path, root);
            }
        }
        removed.push(format!("{} '{}'", item.kind.label(), item.name));
    }
    Ok(removed)
}

/// Install every bundled kind into the global tier and project the
/// canonical store into each supported harness's global dir. Idempotent.
pub fn install_global() -> Result<Vec<String>> {
    install_global_in(&GlobalStore::resolve()?)
}

pub fn install_global_in(store: &GlobalStore) -> Result<Vec<String>> {
    let disabled = Disabled::load_in(&store.config_dir)?;
    let mut lines: Vec<String> = remove_disabled(store, &disabled, false)?
        .into_iter()
        .map(|item| format!("Removed disabled {item} (global)"))
        .collect();
    for kind in BundledKind::ALL {
        let label = kind.label();
        for (item, applied) in install_items(&store.dir(kind), enabled_items(kind, &disabled))? {
            let verb = match applied {
                Applied::Installed => "Installed",
                Applied::Rewrote => "Rewrote",
                // An upgrade reports only what it changed.
                Applied::UpToDate => continue,
            };
            lines.push(format!("{verb} {label} '{}' (global)", item.name));
        }
    }
    lines.extend(state_gitignore::sync_global_registry_ignore(
        &store.config_dir,
        false,
    )?);
    lines.extend(project_global(store, false)?);
    Ok(lines)
}

/// Dry-run of [`install_global`]: `Would …` lines only, nothing written.
pub fn install_global_dry_run() -> Result<Vec<String>> {
    install_global_dry_run_in(&GlobalStore::resolve()?)
}

pub fn install_global_dry_run_in(store: &GlobalStore) -> Result<Vec<String>> {
    let disabled = Disabled::load_in(&store.config_dir)?;
    let mut lines: Vec<String> = remove_disabled(store, &disabled, true)?
        .into_iter()
        .map(|item| format!("Would remove disabled {item} (global)"))
        .collect();
    for kind in BundledKind::ALL {
        for line in install_items_dry_run(&store.dir(kind), enabled_items(kind, &disabled))? {
            lines.push(format!("{line} (global)"));
        }
    }
    lines.extend(state_gitignore::sync_global_registry_ignore(
        &store.config_dir,
        true,
    )?);
    // The projection diffs the canonical store against the harness dir, so
    // diff the store as the install would leave it, not as it is now.
    let staged = tempfile::tempdir()?;
    let staged_store = GlobalStore::at(staged.path());
    for kind in BundledKind::ALL {
        let src = store.dir(kind);
        if kind.store_subdir().is_some() && src.is_dir() {
            copy_dir_recursive(&src, &staged_store.dir(kind))?;
        }
        install_items(&staged_store.dir(kind), enabled_items(kind, &disabled))?;
    }
    remove_disabled(&staged_store, &disabled, false)?;
    lines.extend(project_global_from(
        &staged_store.canonical(),
        &store.home,
        true,
    )?);
    Ok(lines)
}

/// Install `name` (or every item) of `kind` into the global tier. A named
/// item `[bundled.disable]` lists is refused; without a name, disabled items are
/// skipped with a line saying so.
pub(super) fn install_kind_global(kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    install_kind_global_in(&GlobalStore::resolve()?, kind, name)
}

fn install_kind_global_in(
    store: &GlobalStore,
    kind: BundledKind,
    name: Option<&str>,
) -> Result<Vec<String>> {
    let disabled = Disabled::load_in(&store.config_dir)?;
    let mut messages = Vec::new();
    match name {
        Some(n) if disabled.contains(kind, n) => {
            return Err(PmError::SafetyCheck(format!(
                "{} '{n}' is disabled by `[bundled.disable]` in the global pm config; remove it from \
                 that list to install it.",
                kind.label()
            )));
        }
        Some(_) => messages.extend(install_messages(&store.dir(kind), kind, name)?),
        None => {
            for item in items_of_kind(kind).filter(|i| disabled.contains(kind, i.name)) {
                messages.push(format!(
                    "Skipped {} '{}' (disabled by [bundled.disable])",
                    kind.label(),
                    item.name
                ));
            }
            let dir = store.dir(kind);
            for item in enabled_items(kind, &disabled) {
                messages.extend(install_messages(&dir, kind, Some(item.name))?);
            }
        }
    }
    messages.extend(project_global(store, false)?);
    Ok(messages)
}

/// Uninstall `name` (or every item) of `kind` from the global tier and its
/// projections, reporting on the global tier.
pub(super) fn uninstall_global(kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    uninstall_global_in(&GlobalStore::resolve()?, kind, name)
}

fn uninstall_global_in(
    store: &GlobalStore,
    kind: BundledKind,
    name: Option<&str>,
) -> Result<Vec<String>> {
    let messages = uninstall_in(&store.dir(kind), kind, name)?;
    for dir in global_projection_dirs(store, kind) {
        if dir.is_dir() {
            uninstall_in(&dir, kind, name)?;
        }
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::skills::audit::{
        disabled_still_installed_in, global_store_missing_in, unprojected_global_definitions_in,
    };
    use crate::commands::skills::bundled_workflow_names;
    use crate::commands::skills::test_support::*;

    /// A registry repo that committed a bundled workflow (as pre-fix
    /// releases did) and a custom one.
    fn registry_with_committed_workflows(store: &GlobalStore, bundled: &str) {
        crate::git::init_repo(&store.config_dir).unwrap();
        for name in [bundled, "mine"] {
            let dir = store.workflows_dir().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config.toml"), "stale\n").unwrap();
        }
        crate::git::add_all(&store.config_dir).unwrap();
        crate::git::commit(&store.config_dir, "old release").unwrap();
    }

    #[test]
    fn install_global_untracks_committed_bundled_workflows_and_keeps_customs() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        let names = bundled_workflow_names();
        let bundled = names[0];
        registry_with_committed_workflows(&store, bundled);
        let repo = &store.config_dir;
        assert!(!crate::git::ls_files(repo, "workflows").unwrap().is_empty());

        let lines = install_global_in(&store).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(&format!("Untracked workflows/{bundled} from"))),
            "{lines:?}"
        );

        let tracked = crate::git::ls_files(repo, "workflows").unwrap();
        assert_eq!(tracked, vec!["workflows/mine/config.toml".to_string()]);
        // Untracked, not deleted — and the install rewrote it.
        for name in &names {
            assert!(
                store
                    .workflows_dir()
                    .join(name)
                    .join("config.toml")
                    .is_file()
            );
        }

        // The next push is clean: `add -A` re-adds nothing bundled.
        crate::git::add_all(repo).unwrap();
        crate::git::commit(repo, "sync").unwrap();
        assert_eq!(crate::git::status_short(repo).unwrap().trim(), "");
        assert_eq!(
            crate::git::ls_files(repo, "workflows").unwrap(),
            vec!["workflows/mine/config.toml".to_string()]
        );

        let again = install_global_in(&store).unwrap();
        assert!(
            !again.iter().any(|l| l.starts_with("Untracked")),
            "{again:?}"
        );
    }

    #[test]
    fn install_global_dry_run_reports_untrack_without_touching_the_index() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        let bundled = bundled_workflow_names()[0];
        registry_with_committed_workflows(&store, bundled);
        let before = crate::git::ls_files(&store.config_dir, "workflows").unwrap();

        let lines = install_global_dry_run_in(&store).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(&format!("Would untrack workflows/{bundled} from"))),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("Would update")),
            "{lines:?}"
        );
        assert_eq!(
            crate::git::ls_files(&store.config_dir, "workflows").unwrap(),
            before
        );
        assert!(!store.config_dir.join(".gitignore").exists());
    }

    #[test]
    fn install_global_writes_every_kind_and_projects_into_harness_dirs() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        assert_eq!(global_store_missing_in(&store).len(), BUNDLED_ITEMS.len());

        // With nothing installed yet the preview still names the projection
        // the install triggers, with the same file count.
        let dry = install_global_dry_run_in(&store).unwrap();
        let would_project: Vec<&str> = dry
            .iter()
            .filter_map(|l| l.strip_prefix("Would project"))
            .collect();
        assert_eq!(would_project.len(), 2, "{dry:?}");
        assert!(!home.path().join(".agents").exists());
        assert!(!home.path().join(".claude").exists());
        assert!(!home.path().join(".config/opencode").exists());

        let lines = install_global_in(&store).unwrap();
        let projected: Vec<&str> = lines
            .iter()
            .filter_map(|l| l.strip_prefix("Projected"))
            .collect();
        assert_eq!(would_project, projected, "{dry:?} vs {lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l == "Installed Agent 'reviewer' (global)"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l == "Installed Workflow 'solo' (global)"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Projected") && l.contains("for claude-code")),
            "{lines:?}"
        );

        let h = home.path();
        assert!(h.join(".agents/skills/pm/SKILL.md").exists());
        assert!(h.join(".agents/agents/reviewer.md").exists());
        assert!(h.join(".agents/pm-baseline.md").exists());
        assert!(h.join(".config/pm/workflows/solo/config.toml").exists());
        assert_eq!(store.workflows_dir(), h.join(".config/pm/workflows"));
        for harness in Harness::SUPPORTED {
            let dir = harness.global_config_dir(h).unwrap();
            if harness.projects_definitions() {
                assert!(dir.join("agents/reviewer.md").exists(), "{harness}");
                assert_eq!(
                    dir.join("skills/pm/SKILL.md").exists(),
                    harness.projected_dirs().contains(&"skills"),
                    "{harness}"
                );
            } else {
                // codex reads the canonical store itself; nothing lands here.
                assert!(!dir.exists(), "{harness}");
            }
            // The baseline is passed by absolute path, never projected.
            assert!(!dir.join("pm-baseline.md").exists(), "{harness}");
        }
        assert!(global_store_missing_in(&store).is_empty());
        assert!(
            unprojected_global_definitions_in(&store, Harness::SUPPORTED)
                .unwrap()
                .is_empty()
        );

        // Idempotent: nothing left to do.
        assert!(install_global_dry_run_in(&store).unwrap().is_empty());
        assert!(install_global_in(&store).unwrap().is_empty());

        // A hand-edited global bundled file is rewritten on the next install.
        // The harness copy already holds the bundled bytes, so no projection
        // follows the rewrite, and the preview says so.
        fs::write(h.join(".agents/agents/reviewer.md"), "edited").unwrap();
        let dry = install_global_dry_run_in(&store).unwrap();
        assert_eq!(dry, vec!["Would update Agent 'reviewer' (global)"]);
        assert_eq!(
            fs::read_to_string(h.join(".agents/agents/reviewer.md")).unwrap(),
            "edited"
        );
        let lines = install_global_in(&store).unwrap();
        assert_eq!(lines, vec!["Rewrote Agent 'reviewer' (global)"]);
        assert_eq!(
            fs::read_to_string(h.join(".agents/agents/reviewer.md")).unwrap(),
            item(BundledKind::Agent, "reviewer").files[0].1
        );
    }

    #[test]
    fn install_global_removes_disabled_items_everywhere_and_reinstalls_when_reenabled() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let store = GlobalStore::at(h);
        install_global_in(&store).unwrap();
        let gone = [
            ".agents/agents/qa.md",
            ".claude/agents/qa.md",
            ".config/opencode/agents/qa.md",
            ".agents/skills/pm",
            ".claude/skills/pm",
            ".config/pm/workflows/research-only",
            ".agents/pm-baseline.md",
        ];
        for path in gone {
            assert!(h.join(path).exists(), "{path}");
        }
        write_global_config(
            &store,
            "[bundled.disable]\nagents = [\"qa\"]\nskills = [\"pm\"]\nworkflows = [\"research-only\"]\n\
             baseline = true\n",
        );

        let dry = install_global_dry_run_in(&store).unwrap();
        assert_eq!(
            dry,
            vec![
                "Would remove disabled Skill 'pm' (global)",
                "Would remove disabled Agent 'qa' (global)",
                "Would remove disabled Baseline 'pm-baseline' (global)",
                "Would remove disabled Workflow 'research-only' (global)",
            ]
        );
        assert!(h.join(".claude/agents/qa.md").exists());

        let lines = install_global_in(&store).unwrap();
        assert_eq!(
            lines,
            dry.iter()
                .map(|l| l.replace("Would remove", "Removed"))
                .collect::<Vec<_>>()
        );
        for path in gone {
            assert!(!h.join(path).exists(), "{path}");
        }
        for path in [
            ".agents/agents/reviewer.md",
            ".claude/agents/reviewer.md",
            ".agents/skills/messaging/SKILL.md",
            ".config/pm/workflows/solo/config.toml",
        ] {
            assert!(h.join(path).exists(), "{path}");
        }
        assert!(global_store_missing_in(&store).is_empty());
        assert!(install_global_in(&store).unwrap().is_empty());
        assert!(install_global_dry_run_in(&store).unwrap().is_empty());

        write_global_config(&store, "[bundled.disable]\nagents = [\"qa\"]\n");
        assert_eq!(
            global_store_missing_in(&store),
            vec![
                "Skill 'pm'",
                "Baseline 'pm-baseline'",
                "Workflow 'research-only'"
            ]
        );
        install_global_in(&store).unwrap();
        assert!(h.join(".claude/skills/pm/SKILL.md").exists());
        assert!(!h.join(".claude/agents/qa.md").exists());
    }

    #[test]
    fn explicit_install_refuses_a_disabled_name_and_skips_disabled_ones_for_all() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        write_global_config(&store, "[bundled.disable]\nagents = [\"qa\"]\n");

        let err = install_kind_global_in(&store, BundledKind::Agent, Some("qa")).unwrap_err();
        assert!(err.to_string().contains("'qa' is disabled"), "{err}");
        assert!(!home.path().join(".agents/agents/qa.md").exists());

        let lines = install_kind_global_in(&store, BundledKind::Agent, None).unwrap();
        assert!(
            lines.contains(&"Skipped Agent 'qa' (disabled by [bundled.disable])".to_string()),
            "{lines:?}"
        );
        assert!(home.path().join(".agents/agents/reviewer.md").exists());
        assert!(!home.path().join(".agents/agents/qa.md").exists());
        assert!(!home.path().join(".claude/agents/qa.md").exists());
    }

    #[test]
    fn still_installed_report_survives_a_malformed_config() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        install_global_in(&store).unwrap();
        write_global_config(&store, "[bundled.disable]\nagents = [\"qa\"]\n");
        assert_eq!(
            disabled_still_installed_in(&store).unwrap(),
            vec!["Agent 'qa'"]
        );
        write_global_config(&store, "[bundled.disable]\nagents = \"qa\"\n");
        assert!(disabled_still_installed_in(&store).unwrap().is_empty());
    }

    #[test]
    fn install_global_refuses_a_malformed_config_rather_than_reinstall_everything() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        write_global_config(&store, "[bundled.disable]\nagents = \"qa\"\n");
        assert!(install_global_in(&store).is_err());
        assert!(install_global_dry_run_in(&store).is_err());
        assert!(!home.path().join(".agents").exists());
    }

    #[test]
    fn global_uninstall_removes_projections_and_reports_unprojected_customs() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        install_global_in(&store).unwrap();

        uninstall_global_in(&store, BundledKind::Agent, Some("reviewer")).unwrap();
        for h in Harness::SUPPORTED {
            assert!(
                !h.global_config_dir(home.path())
                    .unwrap()
                    .join("agents/reviewer.md")
                    .exists()
            );
        }
        // Only a projecting harness can have an unprojected definition.
        assert!(
            unprojected_global_definitions_in(&store, &[Harness::Codex])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            global_store_missing_in(&store),
            vec!["Agent 'reviewer'".to_string()]
        );

        // A user's global custom def is flagged until projected.
        fs::write(home.path().join(".agents/agents/planner.md"), "# planner").unwrap();
        assert_eq!(
            unprojected_global_definitions_in(&store, Harness::SUPPORTED).unwrap(),
            vec![
                ("planner".to_string(), Harness::ClaudeCode),
                ("planner".to_string(), Harness::OpenCode),
            ]
        );
        assert!(
            unprojected_global_definitions_in(&store, &[])
                .unwrap()
                .is_empty(),
            "a harness the project doesn't use raises no finding"
        );
        install_global_in(&store).unwrap();
        assert!(
            unprojected_global_definitions_in(&store, Harness::SUPPORTED)
                .unwrap()
                .is_empty()
        );
        for projected in [
            ".claude/agents/planner.md",
            ".config/opencode/agents/planner.md",
        ] {
            assert_eq!(
                fs::read_to_string(home.path().join(projected)).unwrap(),
                "# planner",
                "{projected}"
            );
        }
    }
}
