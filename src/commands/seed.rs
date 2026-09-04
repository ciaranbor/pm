//! Seed a feature worktree with what its agents need from main: the
//! projected subdirs of the canonical `.agents/` store and, per harness in
//! use, the harness's own per-worktree files and projected assets
//! (harnesses resolve skills and agent definitions from the worktree they
//! run in, never from main). Copy-only — nothing in the feature is ever
//! deleted.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::fs_utils::{sync_file, sync_tree};
use crate::state::paths;

use super::skills::{self, CANONICAL_DIR};

/// Called during `feat new` / `feat adopt` / `pm upgrade`.
pub fn seed_feature_assets(project_root: &Path, feature_worktree: &Path) -> Result<()> {
    sync_feature(project_root, feature_worktree, false).map(|_| ())
}

/// Whether [`seed_feature_assets`] would change anything in the feature:
/// a seeded file missing or differing. Feature-only files don't count.
pub fn seed_feature_assets_would_change(
    project_root: &Path,
    feature_worktree: &Path,
) -> Result<bool> {
    sync_feature(project_root, feature_worktree, true)
}

/// Returns whether anything was (or, with `dry_run`, would be) written; in
/// `dry_run` it stops at the first difference.
fn sync_feature(project_root: &Path, feature_worktree: &Path, dry_run: bool) -> Result<bool> {
    let main = paths::main_worktree(project_root);
    let mut changed = false;
    for harness in skills::harnesses_in_use(project_root) {
        let cfg = harness.config_dir();
        let mut dirs: Vec<PathBuf> = Vec::new();
        for sub in harness.projected_dirs() {
            dirs.push(Path::new(CANONICAL_DIR).join(sub));
            dirs.push(Path::new(cfg).join(sub));
        }
        for rel in dirs {
            let src = main.join(&rel);
            if src.is_dir() && !sync_tree(&src, &feature_worktree.join(&rel), dry_run)?.is_empty() {
                changed = true;
                if dry_run {
                    return Ok(true);
                }
            }
        }
        for file in harness.seeded_files() {
            let rel = Path::new(cfg).join(file);
            let src = main.join(&rel);
            if src.exists() && sync_file(&src, &feature_worktree.join(&rel), dry_run)? {
                changed = true;
                if dry_run {
                    return Ok(true);
                }
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn write(dir: &Path, filename: &str, content: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(filename), content).unwrap();
    }

    #[test]
    fn seed_copies_settings_from_main_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());

        let main_claude = paths::main_worktree(&project).join(".claude");
        write(&main_claude, "settings.json", r#"{"permissions":true}"#);
        write(&main_claude, "settings.local.json", r#"{"local":true}"#);

        let feature_wt = project.join("login");
        std::fs::create_dir_all(&feature_wt).unwrap();
        seed_feature_assets(&project, &feature_wt).unwrap();

        let dst = feature_wt.join(".claude");
        assert_eq!(
            std::fs::read_to_string(dst.join("settings.json")).unwrap(),
            r#"{"permissions":true}"#
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("settings.local.json")).unwrap(),
            r#"{"local":true}"#
        );
    }

    #[test]
    fn seed_noop_when_main_has_neither_store() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        let main = paths::main_worktree(&project);
        let _ = std::fs::remove_dir_all(main.join(".claude"));
        let _ = std::fs::remove_dir_all(main.join(".agents"));

        let feature_wt = project.join("login");
        std::fs::create_dir_all(&feature_wt).unwrap();
        seed_feature_assets(&project, &feature_wt).unwrap();

        assert!(!feature_wt.join(".claude").exists());
        assert!(!feature_wt.join(".agents").exists());
        assert!(!seed_feature_assets_would_change(&project, &feature_wt).unwrap());
    }

    #[test]
    fn seed_copies_only_existing_settings_files() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());

        let main_claude = paths::main_worktree(&project).join(".claude");
        write(&main_claude, "settings.json", r#"{"only":"this"}"#);

        let feature_wt = project.join("login");
        std::fs::create_dir_all(&feature_wt).unwrap();
        seed_feature_assets(&project, &feature_wt).unwrap();

        let dst = feature_wt.join(".claude");
        assert!(dst.join("settings.json").exists());
        assert!(!dst.join("settings.local.json").exists());
    }

    #[test]
    fn seed_copies_canonical_store_and_harness_projection() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        let main = paths::main_worktree(&project);

        write(&main.join(".agents/agents"), "reviewer.md", "# canonical");
        write(
            &main.join(".agents/skills/pm"),
            "SKILL.md",
            "# canonical skill",
        );
        write(&main.join(".claude/agents"), "reviewer.md", "# projected");
        write(
            &main.join(".claude/skills/pm"),
            "SKILL.md",
            "# projected skill",
        );

        let feature_wt = project.join("login");
        std::fs::create_dir_all(&feature_wt).unwrap();
        assert!(seed_feature_assets_would_change(&project, &feature_wt).unwrap());
        seed_feature_assets(&project, &feature_wt).unwrap();

        for (rel, expected) in [
            (".agents/agents/reviewer.md", "# canonical"),
            (".agents/skills/pm/SKILL.md", "# canonical skill"),
            (".claude/agents/reviewer.md", "# projected"),
            (".claude/skills/pm/SKILL.md", "# projected skill"),
        ] {
            assert_eq!(
                std::fs::read_to_string(feature_wt.join(rel)).unwrap(),
                expected,
                "{rel}"
            );
        }
        // The baseline is applied from main by absolute path, never seeded.
        assert!(!feature_wt.join(".agents/pm-baseline.md").exists());
        assert!(!seed_feature_assets_would_change(&project, &feature_wt).unwrap());
    }

    #[test]
    fn would_change_detects_drift_in_either_store() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        let main = paths::main_worktree(&project);
        let feature_wt = project.join("login");
        std::fs::create_dir_all(&feature_wt).unwrap();
        seed_feature_assets(&project, &feature_wt).unwrap();
        assert!(!seed_feature_assets_would_change(&project, &feature_wt).unwrap());

        write(&main.join(".agents/agents"), "extra.md", "new");
        assert!(seed_feature_assets_would_change(&project, &feature_wt).unwrap());
        seed_feature_assets(&project, &feature_wt).unwrap();
        assert!(!seed_feature_assets_would_change(&project, &feature_wt).unwrap());

        std::fs::write(main.join(".claude/settings.json"), "{\"changed\":1}").unwrap();
        assert!(seed_feature_assets_would_change(&project, &feature_wt).unwrap());
    }

    #[test]
    fn seed_overwrites_stale_copies_and_keeps_feature_only_files() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        let main = paths::main_worktree(&project);
        write(
            &main.join(".claude/skills/pm"),
            "SKILL.md",
            "updated content",
        );
        write(
            &main.join(".agents/skills/pm"),
            "SKILL.md",
            "updated content",
        );

        let feature_wt = project.join("login");
        write(
            &feature_wt.join(".claude/skills/pm"),
            "SKILL.md",
            "old content",
        );
        write(
            &feature_wt.join(".claude/agents"),
            "local-only.md",
            "keep me",
        );
        write(
            &feature_wt.join(".agents/agents"),
            "local-only.md",
            "keep me",
        );

        seed_feature_assets(&project, &feature_wt).unwrap();

        assert_eq!(
            std::fs::read_to_string(feature_wt.join(".claude/skills/pm/SKILL.md")).unwrap(),
            "updated content"
        );
        assert_eq!(
            std::fs::read_to_string(feature_wt.join(".agents/skills/pm/SKILL.md")).unwrap(),
            "updated content"
        );
        assert!(feature_wt.join(".claude/agents/local-only.md").exists());
        assert!(feature_wt.join(".agents/agents/local-only.md").exists());
        // Feature-only files never count as drift.
        assert!(!seed_feature_assets_would_change(&project, &feature_wt).unwrap());
    }
}
