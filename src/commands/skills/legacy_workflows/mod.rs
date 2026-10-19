//! The bundled workflows' old home, `<config dir>/workflows/`, where pm
//! installed them before they moved to the data dir. Every global install
//! deletes a workflow's copy there when each of its files holds content pm
//! shipped (`shipped.rs`); a copy with an edited or added file is kept whole
//! and reported, by the install and by `pm doctor`, since only the user can
//! say where the edit belongs.

mod shipped;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::error::Result;
use crate::hash::sha256_hex;

use super::GlobalStore;
use super::install::prune_empty_parents;
use shipped::SHIPPED;

/// The old dir's bundled workflow copies, by workflow name.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LegacyCopies {
    /// Copies whose every file pm shipped: safe to delete.
    pub shipped: Vec<String>,
    /// Copies with a file pm never shipped, as their dirs.
    pub edited: Vec<PathBuf>,
}

/// A file relative to the old dir, with the SHA-256 of every content pm
/// shipped for it.
pub(crate) type Shipped<'a> = &'a [(&'a str, &'a [&'a str])];

/// One workflow's old copy: every file in its dir, and whether each holds
/// content `table` lists for it. A file the table doesn't name never does.
struct OldCopy {
    dir: PathBuf,
    files: Vec<(PathBuf, bool)>,
}

/// The old copy of each workflow `table` names that has a dir.
fn old_copies<'a>(store: &GlobalStore, table: Shipped<'a>) -> Result<BTreeMap<&'a str, OldCopy>> {
    let root = store.legacy_workflows_dir();
    let mut out = BTreeMap::new();
    for name in table.iter().filter_map(|(rel, _)| rel.split('/').next()) {
        let dir = root.join(name);
        if out.contains_key(name) || !dir.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let rel = format!(
                "{name}/{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            let shipped = path.is_file()
                && table.iter().any(|(r, hashes)| {
                    *r == rel
                        && fs::read(&path)
                            .is_ok_and(|bytes| hashes.contains(&sha256_hex(&bytes).as_str()))
                });
            files.push((path, shipped));
        }
        out.insert(name, OldCopy { dir, files });
    }
    Ok(out)
}

fn classify(copies: &BTreeMap<&str, OldCopy>) -> LegacyCopies {
    let mut out = LegacyCopies::default();
    for (name, copy) in copies {
        if copy.files.iter().all(|(_, shipped)| *shipped) {
            out.shipped.push(name.to_string());
        } else {
            out.edited.push(copy.dir.clone());
        }
    }
    out
}

/// What the old dir holds of the bundled workflows.
pub fn legacy_copies(store: &GlobalStore) -> Result<LegacyCopies> {
    legacy_copies_of(store, SHIPPED)
}

/// [`legacy_copies`] against an explicit shipped table.
pub(crate) fn legacy_copies_of(store: &GlobalStore, table: Shipped) -> Result<LegacyCopies> {
    Ok(classify(&old_copies(store, table)?))
}

/// Delete the old copies pm shipped (none in `dry_run`), returning what the
/// old dir held beforehand.
pub(super) fn remove_legacy_copies(store: &GlobalStore, dry_run: bool) -> Result<LegacyCopies> {
    remove_legacy_copies_of(store, SHIPPED, dry_run)
}

fn remove_legacy_copies_of(
    store: &GlobalStore,
    table: Shipped,
    dry_run: bool,
) -> Result<LegacyCopies> {
    let copies = old_copies(store, table)?;
    let classified = classify(&copies);
    if !dry_run {
        for name in &classified.shipped {
            for (path, _) in &copies[name.as_str()].files {
                fs::remove_file(path)?;
                prune_empty_parents(path, &store.config_dir);
            }
        }
    }
    Ok(classified)
}

/// The install's report on [`remove_legacy_copies`]: one line per removed
/// copy and one per kept copy.
pub(super) fn report(copies: &LegacyCopies, dry_run: bool) -> Vec<String> {
    let verb = if dry_run { "Would remove" } else { "Removed" };
    let mut lines: Vec<String> = copies
        .shipped
        .iter()
        .map(|name| format!("{verb} the old copy of Workflow '{name}' from the pm config dir"))
        .collect();
    lines.extend(copies.edited.iter().map(|dir| {
        format!(
            "Kept {}: edited, and pm no longer updates it there \
             (move the edits to a project's .pm/workflows/, then delete it)",
            dir.display()
        )
    }));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "old release\n";

    /// Each of `rels` with [`OLD`] as its one shipped content.
    fn with_table(rels: &[&str], f: impl FnOnce(Shipped)) {
        let hash = sha256_hex(OLD.as_bytes());
        let hashes = [hash.as_str()];
        let table: Vec<(&str, &[&str])> = rels.iter().map(|rel| (*rel, &hashes[..])).collect();
        f(&table);
    }

    fn write_old(store: &GlobalStore, rel: &str, content: &str) -> PathBuf {
        let path = store.legacy_workflows_dir().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn removes_shipped_copies_and_keeps_an_edited_or_added_to_one_whole() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        write_old(&store, "solo/config.toml", OLD);
        write_old(&store, "solo/workflow.md", OLD);
        let pr_config = write_old(&store, "pr-review/config.toml", OLD);
        let pr_md = write_old(&store, "pr-review/workflow.md", "# my edits\n");
        let custom = write_old(&store, "mine/config.toml", "description = \"x\"\n");
        let ro_config = write_old(&store, "research-only/config.toml", OLD);
        let ro_notes = write_old(&store, "research-only/notes.md", "mine\n");

        let rels = [
            "solo/config.toml",
            "solo/workflow.md",
            "pr-review/config.toml",
            "pr-review/workflow.md",
            "research-only/config.toml",
        ];
        with_table(&rels, |table| {
            let dry = remove_legacy_copies_of(&store, table, true).unwrap();
            assert!(store.legacy_workflows_dir().join("solo").is_dir());

            let copies = remove_legacy_copies_of(&store, table, false).unwrap();
            assert_eq!(copies, dry);
            assert_eq!(copies.shipped, vec!["solo".to_string()]);
            assert_eq!(
                copies.edited,
                vec![
                    store.legacy_workflows_dir().join("pr-review"),
                    store.legacy_workflows_dir().join("research-only")
                ]
            );
            assert!(!store.legacy_workflows_dir().join("solo").exists());
            for kept in [&pr_config, &pr_md, &custom, &ro_config, &ro_notes] {
                assert!(kept.is_file(), "{}", kept.display());
            }

            assert_eq!(
                classify(&old_copies(&store, table).unwrap()),
                LegacyCopies {
                    shipped: Vec::new(),
                    edited: copies.edited,
                }
            );
        });
    }

    #[test]
    fn an_emptied_old_dir_is_pruned_but_the_config_dir_stays() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        write_old(&store, "research-only/config.toml", OLD);

        with_table(&["research-only/config.toml"], |table| {
            remove_legacy_copies_of(&store, table, false).unwrap();
        });
        assert!(!store.legacy_workflows_dir().exists());
        assert!(store.config_dir.is_dir());
    }
}
