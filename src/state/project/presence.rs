//! Whether a registered project is on this machine.
//!
//! The registry syncs between machines through the global state repo, so an
//! entry can name a project that was never restored here. Presence is read
//! from the disk, never stored in the registry: a stored flag would sync to
//! machines it isn't true for.
//!
//! A project is here when `<root>/main` is a directory (a symlink to one
//! counts; a dangling one does not). A root holding only `.pm/` is *not
//! restored*: something wrote state there without a checkout, and
//! `pm restore` completes it. Every registry consumer goes through this rule
//! before touching a root, so nothing writes under a project that isn't here.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::state::paths;

use super::ProjectEntry;

/// Where a registered project's root stands on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Here,
    /// The root exists but holds no main checkout.
    NotRestored,
    RootMissing,
}

impl Presence {
    /// The presence of the project rooted at `root`.
    pub fn of(root: &Path) -> Self {
        if paths::main_worktree(root).is_dir() {
            Self::Here
        } else if root.exists() {
            Self::NotRestored
        } else {
            Self::RootMissing
        }
    }

    pub fn is_here(self) -> bool {
        self == Self::Here
    }
}

impl ProjectEntry {
    pub fn presence(&self) -> Presence {
        Presence::of(&self.root_path())
    }

    /// Load the project `name` and its root, refusing with
    /// [`PmError::NotHere`] when it isn't on this machine.
    pub fn load_here(projects_dir: &Path, name: &str) -> Result<(Self, PathBuf)> {
        let entry = Self::load(projects_dir, name)?;
        let root = entry.root_path();
        if !Presence::of(&root).is_here() {
            return Err(PmError::NotHere {
                name: name.to_string(),
            });
        }
        Ok((entry, root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn presence_distinguishes_here_husk_and_missing_root() {
        let dir = tempdir().unwrap();
        let here = dir.path().join("here");
        std::fs::create_dir_all(here.join("main")).unwrap();
        let husk = dir.path().join("husk");
        std::fs::create_dir_all(husk.join(".pm/messages")).unwrap();
        let dangling = dir.path().join("dangling");
        std::fs::create_dir_all(&dangling).unwrap();
        std::os::unix::fs::symlink(dir.path().join("gone"), dangling.join("main")).unwrap();

        assert_eq!(Presence::of(&here), Presence::Here);
        assert_eq!(Presence::of(&husk), Presence::NotRestored);
        assert_eq!(Presence::of(&dangling), Presence::NotRestored);
        assert_eq!(
            Presence::of(&dir.path().join("missing")),
            Presence::RootMissing
        );
    }

    #[test]
    fn load_here_refuses_a_project_that_is_not_here() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let husk = dir.path().join("husk");
        std::fs::create_dir_all(husk.join(".pm")).unwrap();
        register(&projects_dir, "husk", &husk);
        register(&projects_dir, "gone", &dir.path().join("gone"));

        for name in ["husk", "gone"] {
            let err = ProjectEntry::load_here(&projects_dir, name).unwrap_err();
            assert!(matches!(&err, PmError::NotHere { name: n } if n == name));
            assert!(err.to_string().contains(&format!("pm restore {name}")));
        }

        std::fs::create_dir_all(husk.join("main")).unwrap();
        let (_, root) = ProjectEntry::load_here(&projects_dir, "husk").unwrap();
        assert_eq!(root, husk);
    }
}
