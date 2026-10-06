//! Projecting a canonical asset store into a harness's own layout.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::fs_utils;

/// What a projection wrote (or, in dry-run, would write), as paths
/// relative to the target root.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Projection {
    pub written: Vec<PathBuf>,
    /// The subset of `written` that replaced an existing file with
    /// different content.
    pub replaced: Vec<PathBuf>,
}

impl Projection {
    pub fn is_empty(&self) -> bool {
        self.written.is_empty()
    }
}

/// Which part of a projection
/// [`Harness::project_assets`](super::Harness::project_assets) writes.
#[derive(Debug, Default)]
pub struct ProjectionScope<'a> {
    /// The projected subdirs to write; `None` for all of the harness's.
    pub subdirs: Option<&'a [&'a str]>,
    /// Target paths, relative to the target root, never written (a
    /// directory: its whole subtree).
    pub keep: HashSet<PathBuf>,
}

/// Copy `<canonical_root>/<subdir>` over `<target_root>/<subdir>` for each
/// `subdirs` entry within `scope`, recording what changed. Shared by every
/// harness whose projection is a plain copy.
pub(crate) fn project_by_copy(
    canonical_root: &Path,
    target_root: &Path,
    subdirs: &[&str],
    scope: &ProjectionScope<'_>,
    dry_run: bool,
) -> Result<Projection> {
    let mut out = Projection::default();
    for sub in subdirs {
        if scope.subdirs.is_some_and(|only| !only.contains(sub)) {
            continue;
        }
        let src = canonical_root.join(sub);
        if !src.is_dir() {
            continue;
        }
        let dst = target_root.join(sub);
        let keep: HashSet<PathBuf> = scope
            .keep
            .iter()
            .filter_map(|p| p.strip_prefix(sub).ok().map(Path::to_path_buf))
            .collect();
        for (rel, replaced) in fs_utils::sync_tree_except(&src, &dst, &keep, dry_run)? {
            let rel = Path::new(sub).join(rel);
            if replaced {
                out.replaced.push(rel.clone());
            }
            out.written.push(rel);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Harness;

    #[test]
    fn project_assets_copies_overwrites_and_never_deletes() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join(".agents");
        let target = tmp.path().join(".claude");
        std::fs::create_dir_all(canonical.join("agents")).unwrap();
        std::fs::create_dir_all(canonical.join("skills/pm")).unwrap();
        std::fs::write(canonical.join("agents/reviewer.md"), "new").unwrap();
        std::fs::write(canonical.join("skills/pm/SKILL.md"), "skill").unwrap();
        std::fs::create_dir_all(target.join("agents")).unwrap();
        std::fs::write(target.join("agents/reviewer.md"), "old").unwrap();
        std::fs::write(target.join("agents/custom.md"), "mine").unwrap();

        // Dry run: reports, writes nothing.
        let dry = Harness::ClaudeCode
            .project_assets(&canonical, &target, &ProjectionScope::default(), true)
            .unwrap();
        assert_eq!(dry.written.len(), 2);
        assert_eq!(dry.replaced, vec![PathBuf::from("agents/reviewer.md")]);
        assert_eq!(
            std::fs::read_to_string(target.join("agents/reviewer.md")).unwrap(),
            "old"
        );
        assert!(!target.join("skills").exists());

        let real = Harness::ClaudeCode
            .project_assets(&canonical, &target, &ProjectionScope::default(), false)
            .unwrap();
        assert_eq!(real, dry);
        assert_eq!(
            std::fs::read_to_string(target.join("agents/reviewer.md")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("skills/pm/SKILL.md")).unwrap(),
            "skill"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("agents/custom.md")).unwrap(),
            "mine"
        );

        // In sync: nothing to do.
        let again = Harness::ClaudeCode
            .project_assets(&canonical, &target, &ProjectionScope::default(), true)
            .unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn codex_layout_needs_no_projection_and_hooks_live_in_codex_home() {
        let home = Path::new("/h");
        assert_eq!(
            Harness::Codex.user_settings_file(home),
            Some(PathBuf::from("/h/.codex/hooks.json"))
        );
        assert_eq!(
            Harness::Codex.global_config_dir(home),
            Some(PathBuf::from("/h/.codex"))
        );
        assert!(Harness::Codex.projected_dirs().is_empty());

        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join(".agents");
        std::fs::create_dir_all(canonical.join("agents")).unwrap();
        std::fs::write(canonical.join("agents/reviewer.md"), "x").unwrap();
        let target = tmp.path().join(".codex");
        assert!(
            Harness::Codex
                .project_assets(&canonical, &target, &ProjectionScope::default(), false)
                .unwrap()
                .is_empty()
        );
        assert!(!target.exists());
    }
}
