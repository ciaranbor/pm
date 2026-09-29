//! Bring a feature worktree up to date with what its agents need from main:
//! the canonical `.agents/` store (which codex reads directly) and, per
//! harness in use, the harness's own per-worktree files and projected assets
//! (harnesses resolve skills and agent definitions from the worktree they
//! run in, never from main). Runs when a feature is created and on an
//! explicit `pm harness pull` — never as a side effect of another command,
//! so a live feature changes only when someone asks. Copy-only — nothing in
//! the feature is ever deleted — and a file the feature's branch tracks in
//! git is never written: its content is the branch's, and reaches or leaves
//! main by merge.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use std::collections::HashSet;

use crate::fs_utils::{sync_file, sync_tree_except};
use crate::git;
use crate::state::paths;

use super::skills::{self, CANONICAL_DIR};

/// The canonical store's subdirs every feature needs, whatever its harnesses.
const CANONICAL_SUBDIRS: &[&str] = &["agents", "skills"];

/// Called during `feat new` / `feat adopt` / `feat review`.
pub fn seed_feature_assets(project_root: &Path, feature_worktree: &Path) -> Result<()> {
    sync_feature(project_root, feature_worktree, false).map(|_| ())
}

/// `pm harness pull`: seed an existing feature again. Returns the files
/// written (or, with `dry_run`, that would be), relative to the worktree.
pub fn pull(project_root: &Path, feature_name: &str, dry_run: bool) -> Result<Vec<PathBuf>> {
    super::claude_settings::require_feature(project_root, feature_name)?;
    let worktree = project_root.join(feature_name);
    if !worktree.is_dir() {
        return Err(PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "feature '{feature_name}' has no worktree at {}",
                worktree.display()
            ),
        )));
    }
    sync_feature(project_root, &worktree, dry_run)
}

fn sync_feature(
    project_root: &Path,
    feature_worktree: &Path,
    dry_run: bool,
) -> Result<Vec<PathBuf>> {
    let main = paths::main_worktree(project_root);
    let mut written = Vec::new();
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
        let files = sync_untracked(&main, feature_worktree, &rel, dry_run)?;
        written.extend(files.into_iter().map(|f| rel.join(f)));
    }
    for harness in &harnesses {
        for file in harness.seeded_files() {
            let rel = Path::new(harness.config_dir()).join(file);
            if seed_file(&main, feature_worktree, &rel, dry_run)? == Some(Seeded::Written) {
                written.push(rel);
            }
        }
    }
    Ok(written)
}

/// What [`seed_file`] did with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seeded {
    /// Copied (or, with `dry_run`, would be).
    Written,
    /// Already matched main.
    Unchanged,
    /// Left alone: the worktree's branch tracks it.
    Tracked,
}

/// Copy the file `rel` from `main` into `worktree` unless the worktree's
/// branch tracks it. `None` when main has no such file.
pub fn seed_file(
    main: &Path,
    worktree: &Path,
    rel: &Path,
    dry_run: bool,
) -> Result<Option<Seeded>> {
    let src = main.join(rel);
    if !src.exists() {
        return Ok(None);
    }
    Ok(Some(if !tracked_under(worktree, rel)?.is_empty() {
        Seeded::Tracked
    } else if sync_file(&src, &worktree.join(rel), dry_run)? {
        Seeded::Written
    } else {
        Seeded::Unchanged
    }))
}

/// Sync the directory `rel` from `main` into `worktree`, skipping files the
/// worktree's branch tracks. Returns the files written (or, with `dry_run`,
/// that would be), relative to `rel`; a `rel` main doesn't have is a no-op.
fn sync_untracked(main: &Path, worktree: &Path, rel: &Path, dry_run: bool) -> Result<Vec<PathBuf>> {
    let src = main.join(rel);
    if !src.is_dir() {
        return Ok(Vec::new());
    }
    let keep = tracked_under(worktree, rel)?;
    Ok(sync_tree_except(&src, &worktree.join(rel), &keep, dry_run)?
        .into_iter()
        .map(|(path, _)| path)
        .collect())
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

    fn read(path: PathBuf) -> String {
        std::fs::read_to_string(path).unwrap()
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
        assert_eq!(read(dst.join("settings.json")), r#"{"permissions":true}"#);
        assert!(!dst.join("settings.local.json").exists());
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
        seed_feature_assets(&project, &feature_wt).unwrap();

        for (rel, expected) in [
            (".agents/agents/reviewer.md", "# canonical"),
            (".agents/skills/pm/SKILL.md", "# canonical skill"),
            (".claude/agents/reviewer.md", "# projected"),
            (".claude/skills/pm/SKILL.md", "# projected skill"),
        ] {
            assert_eq!(read(feature_wt.join(rel)), expected, "{rel}");
        }
        // The baseline is applied from main by absolute path, never seeded.
        assert!(!feature_wt.join(".agents/pm-baseline.md").exists());
    }

    #[test]
    fn pull_brings_customs_added_to_main_after_the_feature_was_created() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let main = paths::main_worktree(&project);
        let feature_wt = project.join("login");
        assert!(pull(&project, "login", true).unwrap().is_empty());

        write(&main.join(".agents/agents"), "custom.md", "custom def");
        write(&main.join(".claude/agents"), "custom.md", "custom def");
        write(&main.join(".agents/skills/howto"), "SKILL.md", "skill");
        write(&main.join(".claude/skills/howto"), "SKILL.md", "skill");
        write(&main.join(".claude"), "settings.json", r#"{"changed":1}"#);
        write(&feature_wt.join(".agents/agents"), "local-only.md", "keep");

        let expected: Vec<PathBuf> = [
            ".agents/agents/custom.md",
            ".agents/skills/howto/SKILL.md",
            ".claude/agents/custom.md",
            ".claude/skills/howto/SKILL.md",
            ".claude/settings.json",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();

        assert_eq!(pull(&project, "login", true).unwrap(), expected);
        assert!(
            !feature_wt.join(".agents/agents/custom.md").exists(),
            "dry-run wrote"
        );

        assert_eq!(pull(&project, "login", false).unwrap(), expected);
        assert_eq!(
            read(feature_wt.join(".agents/agents/custom.md")),
            "custom def"
        );
        assert_eq!(
            read(feature_wt.join(".claude/agents/custom.md")),
            "custom def"
        );
        assert_eq!(
            read(feature_wt.join(".claude/skills/howto/SKILL.md")),
            "skill"
        );
        assert_eq!(
            read(feature_wt.join(".claude/settings.json")),
            r#"{"changed":1}"#
        );
        assert!(feature_wt.join(".agents/agents/local-only.md").exists());
        assert!(pull(&project, "login", false).unwrap().is_empty());

        let err = pull(&project, "nonexistent", false).unwrap_err();
        assert!(matches!(err, PmError::FeatureNotFound(_)));

        // A registered feature whose worktree is gone is not recreated.
        std::fs::remove_dir_all(&feature_wt).unwrap();
        write(&main.join(".agents/agents"), "later.md", "later");
        assert!(pull(&project, "login", false).is_err());
        assert!(!feature_wt.exists());
    }

    #[test]
    fn seed_and_pull_leave_files_the_feature_branch_tracks() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let main = paths::main_worktree(&project);
        let feature_wt = project.join("login");
        let tracked = [
            ".agents/skills/custom/SKILL.md",
            ".agents/agents/custom.md",
            ".claude/settings.json",
        ];

        write(&main.join(".agents/skills/custom"), "notes.md", "untracked");
        for rel in tracked {
            let rel = Path::new(rel);
            let name = rel.file_name().unwrap().to_str().unwrap();
            write(&main.join(rel.parent().unwrap()), name, "main's");
            write(
                &feature_wt.join(rel.parent().unwrap()),
                name,
                "edited on the feature branch",
            );
            git::stage_file(&feature_wt, &rel.to_string_lossy()).unwrap();
        }
        git::commit(&feature_wt, "edit customs").unwrap();

        let assert_kept = |step: &str| {
            for rel in tracked {
                assert_eq!(
                    read(feature_wt.join(rel)),
                    "edited on the feature branch",
                    "{step}: {rel}"
                );
            }
        };

        seed_feature_assets(&project, &feature_wt).unwrap();
        assert_kept("seed");
        assert_eq!(
            read(feature_wt.join(".agents/skills/custom/notes.md")),
            "untracked"
        );

        write(&main.join(".agents/skills/custom"), "notes.md", "newer");
        assert_eq!(
            pull(&project, "login", false).unwrap(),
            vec![PathBuf::from(".agents/skills/custom/notes.md")]
        );
        assert_kept("pull");
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
            read(feature_wt.join(".claude/skills/pm/SKILL.md")),
            "updated content"
        );
        assert_eq!(
            read(feature_wt.join(".agents/skills/pm/SKILL.md")),
            "updated content"
        );
        assert!(feature_wt.join(".claude/agents/local-only.md").exists());
        assert!(feature_wt.join(".agents/agents/local-only.md").exists());
    }
}
