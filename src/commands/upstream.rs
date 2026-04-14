//! `pm upstream write` — write an upstream doc from a feature worktree.
//!
//! Writes to `.pm/upstream/<feature>.md` in the project root so the
//! information persists after the feature worktree is deleted.
//!
//! NOTE: This resolves the feature name from CWD, which assumes a flat
//! project layout (project_root/<feature>/). This breaks the recursive
//! nature of pm features (stacked worktrees) but is acceptable as a
//! stopgap until cross-scope messaging is implemented.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::paths;

/// Write (or overwrite) the upstream doc for the current feature.
pub fn write(project_root: &Path, feature: &str, content: &str) -> Result<()> {
    let upstream_dir = paths::pm_dir(project_root).join("upstream");
    std::fs::create_dir_all(&upstream_dir)?;
    let path = upstream_dir.join(format!("{feature}.md"));
    std::fs::write(&path, content)?;
    eprintln!("Wrote upstream doc to {}", path.display());
    Ok(())
}

/// Resolve feature name from CWD via the project root.
pub fn run(content: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let feature =
        paths::detect_feature_from_cwd(&project_root, &cwd).ok_or(PmError::NotInWorktree)?;
    write(&project_root, &feature, content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_project(dir: &Path) -> std::path::PathBuf {
        let root = dir.to_path_buf();
        std::fs::create_dir_all(root.join(".pm/features")).unwrap();
        std::fs::write(
            root.join(".pm/features/my-feat.toml"),
            "[status]\nstatus = \"wip\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("my-feat")).unwrap();
        root
    }

    #[test]
    fn write_creates_upstream_doc() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        write(&root, "my-feat", "some notes").unwrap();

        let path = root.join(".pm/upstream/my-feat.md");
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "some notes");
    }

    #[test]
    fn write_overwrites_existing() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        write(&root, "my-feat", "first").unwrap();
        write(&root, "my-feat", "second").unwrap();

        let content = std::fs::read_to_string(root.join(".pm/upstream/my-feat.md")).unwrap();
        assert_eq!(content, "second");
    }
}
