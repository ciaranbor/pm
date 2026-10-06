//! Install and uninstall primitives over one store directory.

use std::fs;
use std::path::Path;

use crate::error::Result;
use crate::fs_utils::write_atomic;

use super::super::bundled_disable::Disabled;
use super::bundled::{
    BundledItem, BundledKind, find_item, is_installed, is_up_to_date, items_of_kind,
};

pub(super) fn status_label(dir: &Path, item: &BundledItem) -> &'static str {
    if !is_installed(dir, item) {
        "not installed"
    } else if is_up_to_date(dir, item) {
        "installed"
    } else {
        "outdated"
    }
}

/// What installing one bundled item did (or, in a dry run, would do).
/// Rewriting drifted content is the one destructive step here, and pm can't
/// tell a user edit from a bundle change — so callers report *which* items
/// were rewritten without claiming why.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Applied {
    Installed,
    Rewrote,
    UpToDate,
}

/// Install (or rewrite) bundled items of `kind` under `dir`, reporting what
/// each one did. The bundle is authoritative for every kind: an item whose
/// on-disk content differs is overwritten. Paths under `dir` that no bundled
/// item names are never touched.
pub(super) fn install_in(
    dir: &Path,
    kind: BundledKind,
    name: Option<&str>,
) -> Result<Vec<(&'static BundledItem, Applied)>> {
    install_items(dir, items_to_install(kind, name)?)
}

pub(super) fn install_items(
    dir: &Path,
    items: impl IntoIterator<Item = &'static BundledItem>,
) -> Result<Vec<(&'static BundledItem, Applied)>> {
    let mut applied = Vec::new();
    for item in items {
        if is_up_to_date(dir, item) {
            applied.push((item, Applied::UpToDate));
            continue;
        }
        let outcome = if is_installed(dir, item) {
            Applied::Rewrote
        } else {
            Applied::Installed
        };
        for (rel, content) in item.files {
            write_atomic(&dir.join(rel), content.as_bytes())?;
        }
        applied.push((item, outcome));
    }
    Ok(applied)
}

/// One user-facing line per item, including the no-ops — what the explicit
/// install commands print.
pub(super) fn install_messages(
    dir: &Path,
    kind: BundledKind,
    name: Option<&str>,
) -> Result<Vec<String>> {
    let label = kind.label();
    Ok(install_in(dir, kind, name)?
        .into_iter()
        .map(|(item, applied)| match applied {
            Applied::Installed => format!("Installed {label} '{}'", item.name),
            Applied::Rewrote => format!("Rewrote {label} '{}'", item.name),
            Applied::UpToDate => format!("{label} '{}' is already up to date", item.name),
        })
        .collect())
}

/// Dry-run companion to [`install_items`]: one `Would …` line per item whose
/// on-disk content does not match the bundle; up-to-date items produce no
/// output, so every returned line is an action that would be taken.
pub(super) fn install_items_dry_run(
    dir: &Path,
    items: impl IntoIterator<Item = &'static BundledItem>,
) -> Result<Vec<String>> {
    let mut messages = Vec::new();
    for item in items {
        let label = item.kind.label();
        if is_up_to_date(dir, item) {
            continue;
        }
        let verb = if is_installed(dir, item) {
            "update"
        } else {
            "install"
        };
        messages.push(format!("Would {verb} {label} '{}'", item.name));
    }
    Ok(messages)
}

/// The items of `kind` that `disabled` leaves enabled.
pub(super) fn enabled_items(
    kind: BundledKind,
    disabled: &Disabled,
) -> impl Iterator<Item = &'static BundledItem> + '_ {
    items_of_kind(kind).filter(move |i| !disabled.contains(kind, i.name))
}

fn items_to_install(kind: BundledKind, name: Option<&str>) -> Result<Vec<&'static BundledItem>> {
    Ok(match name {
        Some(n) => vec![find_item(kind, n)?],
        None => items_of_kind(kind).collect(),
    })
}

pub(super) fn uninstall_in(
    dir: &Path,
    kind: BundledKind,
    name: Option<&str>,
) -> Result<Vec<String>> {
    let label = kind.label();
    let mut messages = Vec::new();
    for item in items_to_install(kind, name)? {
        if !is_installed(dir, item) {
            messages.push(format!("{label} '{}' is not installed", item.name));
            continue;
        }
        for (rel, _content) in item.files {
            let path = dir.join(rel);
            if path.exists() {
                fs::remove_file(&path)?;
            }
            prune_empty_parents(&path, dir);
        }
        messages.push(format!("Uninstalled {label} '{}'", item.name));
    }
    Ok(messages)
}

/// Remove now-empty directories between `path` and `stop` (exclusive).
pub(super) fn prune_empty_parents(path: &Path, stop: &Path) {
    let mut cur = path.parent();
    while let Some(dir) = cur {
        if dir == stop || !dir.starts_with(stop) {
            break;
        }
        if !dir.read_dir().is_ok_and(|mut d| d.next().is_none()) {
            break;
        }
        let _ = fs::remove_dir(dir);
        cur = dir.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::skills::test_support::*;
    use crate::error::PmError;

    #[test]
    fn install_by_name_and_check_status() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        assert_eq!(
            status_label(&dir, item(BundledKind::Skill, "pm")),
            "not installed"
        );

        let messages = install_messages(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert_eq!(messages, vec!["Installed Skill 'pm'".to_string()]);
        assert_eq!(
            status_label(&dir, item(BundledKind::Skill, "pm")),
            "installed"
        );
        assert_eq!(
            status_label(&dir, item(BundledKind::Skill, "messaging")),
            "not installed"
        );

        let second = install_messages(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(second[0].contains("already up to date"));
        assert!(
            install_items_dry_run(&dir, [item(BundledKind::Skill, "pm")])
                .unwrap()
                .is_empty()
        );

        fs::write(dir.join("pm/SKILL.md"), "old content").unwrap();
        assert_eq!(
            status_label(&dir, item(BundledKind::Skill, "pm")),
            "outdated"
        );
        assert_eq!(
            install_items_dry_run(&dir, [item(BundledKind::Skill, "pm")]).unwrap(),
            vec!["Would update Skill 'pm'".to_string()]
        );
        let third = install_messages(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert_eq!(third, vec!["Rewrote Skill 'pm'".to_string()]);
        assert!(is_up_to_date(&dir, item(BundledKind::Skill, "pm")));
    }

    #[test]
    fn install_all_of_a_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        for kind in BundledKind::ALL {
            let count = items_of_kind(kind).count();
            let applied = install_in(&dir, kind, None).unwrap();
            assert_eq!(applied.len(), count);
            assert!(applied.iter().all(|(_, a)| *a == Applied::Installed));
            for item in items_of_kind(kind) {
                assert!(is_installed(&dir, item), "{}", item.name);
            }
        }
        // Workflows install both files.
        let wf = dir.join("implement-and-review");
        assert!(wf.join("config.toml").exists() && wf.join("workflow.md").exists());
    }

    #[test]
    fn install_unknown_name_fails_per_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        for (kind, check) in [
            (
                BundledKind::Skill,
                (|e: &PmError| matches!(e, PmError::SkillNotFound(_))) as fn(&PmError) -> bool,
            ),
            (BundledKind::Agent, |e| {
                matches!(e, PmError::AgentNotFound(_))
            }),
            (BundledKind::Workflow, |e| {
                matches!(e, PmError::WorkflowNotFound(_))
            }),
        ] {
            let err = install_messages(&dir, kind, Some("nonexistent")).unwrap_err();
            assert!(check(&err), "{err}");
        }
    }

    #[test]
    fn uninstall_removes_files_and_prunes_empty_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        install_in(&dir, BundledKind::Skill, None).unwrap();
        let user_skill = dir.join("mine/SKILL.md");
        fs::create_dir_all(user_skill.parent().unwrap()).unwrap();
        fs::write(&user_skill, "mine").unwrap();

        let messages = uninstall_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert_eq!(messages, vec!["Uninstalled Skill 'pm'".to_string()]);
        assert!(!dir.join("pm").exists());
        assert!(dir.join("messaging/SKILL.md").exists());

        let messages = uninstall_in(&dir, BundledKind::Skill, None).unwrap();
        assert!(messages.iter().any(|m| m.contains("'pm' is not installed")));
        for item in items_of_kind(BundledKind::Skill) {
            assert!(!is_installed(&dir, item));
        }
        assert_eq!(fs::read_to_string(&user_skill).unwrap(), "mine");
        assert!(dir.exists());
    }
}
