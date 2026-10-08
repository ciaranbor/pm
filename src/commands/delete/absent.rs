//! Deleting a registered project by name, including one that isn't on this
//! machine: that one has no config or checkout to check, so it is only
//! unregistered, with its `.pm/` state and then its root removed, the root
//! only when nothing else is in it.

use std::path::Path;

use crate::error::Result;
use crate::state::paths;
use crate::state::project::ProjectEntry;

use super::{Deleted, Pending, delete};

/// [`delete`] the registered project `name`, or, when it isn't on this
/// machine, unregister it as the module doc says.
pub fn delete_named(
    projects_dir: &Path,
    name: &str,
    force: bool,
    tmux_server: Option<&str>,
    confirm: impl FnOnce(&Pending) -> Result<bool>,
) -> Result<Option<Deleted>> {
    let entry = ProjectEntry::load(projects_dir, name)?;
    let root = entry.root_path();
    if entry.presence().is_here() {
        return delete(&root, projects_dir, force, tmux_server, confirm);
    }
    let pm_dir = paths::pm_dir(&root);
    let pending_warnings: Vec<String> = pm_dir
        .exists()
        .then(|| {
            format!(
                "'{name}' is not on this machine: its state at {} is removed too",
                pm_dir.display()
            )
        })
        .into_iter()
        .collect();
    let pending = Pending {
        project: name,
        features: 0,
        main: None,
        warnings: &pending_warnings,
    };
    if !confirm(&pending)? {
        return Ok(None);
    }
    if pm_dir.exists() {
        std::fs::remove_dir_all(&pm_dir)?;
    }
    let mut warnings = Vec::new();
    if root.exists() && std::fs::remove_dir(&root).is_err() {
        warnings.push(format!(
            "left {} in place: it holds more than pm state",
            root.display()
        ));
    }
    std::fs::remove_file(projects_dir.join(format!("{name}.toml")))?;
    Ok(Some(Deleted {
        project: name.to_string(),
        warnings,
        own: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PmError;
    use tempfile::tempdir;

    fn register(projects_dir: &Path, name: &str, root: &Path) {
        ProjectEntry {
            root: root.to_string_lossy().into_owned(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        }
        .save(projects_dir, name)
        .unwrap();
    }

    #[test]
    fn deleting_a_husk_unregisters_it_and_removes_its_root() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let husk = dir.path().join("husk");
        std::fs::create_dir_all(husk.join(".pm/messages/main")).unwrap();
        crate::git::init_repo(&paths::pm_dir(&husk)).unwrap();
        register(&projects_dir, "husk", &husk);

        let deleted = delete_named(&projects_dir, "husk", false, None, |pending| {
            assert_eq!((pending.project, pending.main), ("husk", None));
            assert_eq!(pending.warnings.len(), 1, "{:?}", pending.warnings);
            Ok(true)
        })
        .unwrap()
        .unwrap();

        assert!(deleted.warnings.is_empty(), "{:?}", deleted.warnings);
        assert!(!husk.exists());
        assert!(matches!(
            ProjectEntry::load(&projects_dir, "husk"),
            Err(PmError::ProjectNotFound(_))
        ));
    }

    #[test]
    fn deleting_a_project_not_here_keeps_what_else_its_root_holds() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let root = dir.path().join("mixed");
        std::fs::create_dir_all(paths::pm_dir(&root)).unwrap();
        std::fs::write(root.join("notes.txt"), "mine").unwrap();
        register(&projects_dir, "mixed", &root);
        register(&projects_dir, "gone", &dir.path().join("gone"));

        let deleted = delete_named(&projects_dir, "mixed", false, None, |_| Ok(true))
            .unwrap()
            .unwrap();
        assert_eq!(deleted.warnings.len(), 1, "{:?}", deleted.warnings);
        assert!(root.join("notes.txt").exists());
        assert!(!paths::pm_dir(&root).exists());

        delete_named(&projects_dir, "gone", false, None, |pending| {
            assert!(pending.warnings.is_empty());
            Ok(true)
        })
        .unwrap()
        .unwrap();
        assert!(ProjectEntry::list(&projects_dir).unwrap().is_empty());
    }

    #[test]
    fn a_declined_delete_of_a_husk_changes_nothing() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let husk = dir.path().join("husk");
        std::fs::create_dir_all(paths::pm_dir(&husk)).unwrap();
        register(&projects_dir, "husk", &husk);

        let deleted = delete_named(&projects_dir, "husk", false, None, |_| Ok(false)).unwrap();

        assert!(deleted.is_none());
        assert!(paths::pm_dir(&husk).is_dir());
        assert!(ProjectEntry::load(&projects_dir, "husk").is_ok());
    }
}
