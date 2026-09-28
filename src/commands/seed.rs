//! Seed a feature worktree with what its agents need from main: the
//! canonical `.agents/` store (which codex reads directly) and, per harness
//! in use, the harness's own per-worktree files and projected assets
//! (harnesses resolve skills and agent definitions from the worktree they
//! run in, never from main). Copy-only — nothing in the feature is ever
//! deleted — and a file the feature's branch tracks in git is never written:
//! its content is the branch's, and reaches or leaves main by merge.

use std::path::{Path, PathBuf};

use crate::error::Result;
use std::collections::HashSet;

use crate::fs_utils::{sync_file, sync_tree_except};
use crate::git;
use crate::state::paths;

use super::skills::{self, CANONICAL_DIR};

/// The canonical store's subdirs every feature needs, whatever its harnesses.
const CANONICAL_SUBDIRS: &[&str] = &["agents", "skills"];

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
    let harnesses = skills::harnesses_in_use(project_root)?;
    let mut dirs: Vec<PathBuf> = CANONICAL_SUBDIRS
        .iter()
        .map(|sub| Path::new(CANONICAL_DIR).join(sub))
        .collect();
    for harness in &harnesses {
        let cfg = harness.config_dir();
        for sub in harness.projected_dirs() {
            dirs.push(Path::new(cfg).join(sub));
        }
    }
    for rel in dirs {
        if sync_untracked(&main, feature_worktree, &rel, dry_run)? {
            changed = true;
            if dry_run {
                return Ok(true);
            }
        }
    }
    for harness in &harnesses {
        for file in harness.seeded_files() {
            let rel = Path::new(harness.config_dir()).join(file);
            let src = main.join(&rel);
            if src.exists()
                && tracked_under(feature_worktree, &rel)?.is_empty()
                && sync_file(&src, &feature_worktree.join(&rel), dry_run)?
            {
                changed = true;
                if dry_run {
                    return Ok(true);
                }
            }
        }
    }
    Ok(changed)
}

/// Sync the directory `rel` from `main` into `worktree`, skipping files the
/// worktree's branch tracks. Returns whether anything was (or would be)
/// written; a `rel` main doesn't have is a no-op.
pub(super) fn sync_untracked(
    main: &Path,
    worktree: &Path,
    rel: &Path,
    dry_run: bool,
) -> Result<bool> {
    let src = main.join(rel);
    if !src.is_dir() {
        return Ok(false);
    }
    let keep = tracked_under(worktree, rel)?;
    Ok(!sync_tree_except(&src, &worktree.join(rel), &keep, dry_run)?.is_empty())
}

/// Files under `rel` that `worktree`'s branch tracks, relative to `rel`
/// (the empty path when `rel` is itself a tracked file).
fn tracked_under(worktree: &Path, rel: &Path) -> Result<HashSet<PathBuf>> {
    if !git::is_git_repo(worktree) {
        return Ok(HashSet::new());
    }
    Ok(git::ls_files(worktree, &rel.to_string_lossy())?
        .iter()
        .filter_map(|f| Path::new(f).strip_prefix(rel).ok().map(Path::to_path_buf))
        .collect())
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
    fn seed_copies_settings_json_but_not_settings_local_json() {
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
        assert!(!dst.join("settings.local.json").exists());
        assert!(!seed_feature_assets_would_change(&project, &feature_wt).unwrap());
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

        write(&main.join(".claude"), "settings.json", "{\"changed\":1}");
        assert!(seed_feature_assets_would_change(&project, &feature_wt).unwrap());
    }

    #[test]
    fn seed_and_pull_leave_a_skill_the_feature_branch_tracks() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let main = paths::main_worktree(&project);
        let feature_wt = project.join("login");
        let skill = ".agents/skills/custom/SKILL.md";

        write(&main.join(".agents/skills/custom"), "SKILL.md", "main's");
        write(&main.join(".agents/skills/custom"), "notes.md", "untracked");
        write(
            &feature_wt.join(".agents/skills/custom"),
            "SKILL.md",
            "edited on the feature branch",
        );
        git::stage_file(&feature_wt, skill).unwrap();
        git::commit(&feature_wt, "edit skill").unwrap();

        let assert_kept = |step: &str| {
            assert_eq!(
                std::fs::read_to_string(feature_wt.join(skill)).unwrap(),
                "edited on the feature branch",
                "{step}"
            );
            let status = git::status_short(&feature_wt).unwrap();
            assert!(!status.contains("SKILL.md"), "{step}: {status}");
        };

        seed_feature_assets(&project, &feature_wt).unwrap();
        assert_kept("seed");
        // Files the branch doesn't track are still seeded.
        assert_eq!(
            std::fs::read_to_string(feature_wt.join(".agents/skills/custom/notes.md")).unwrap(),
            "untracked"
        );
        // A tracked file that differs from main's is not drift.
        assert!(!seed_feature_assets_would_change(&project, &feature_wt).unwrap());

        skills::skills_pull(&project, "login").unwrap();
        assert_kept("skills pull");
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
