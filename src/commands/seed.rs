//! Bring a feature worktree up to date with what its agents need, per
//! harness in use: the harness's own per-worktree files and projected
//! assets (harnesses resolve skills and agent definitions from the worktree
//! they run in, never from main). Skills are the feature's own: its
//! canonical `.agents/skills` (which codex and opencode read directly) is
//! seeded from main, then projected into the harness's dir, so a skill
//! edited on the branch reaches its agents at the next seed or pull. Agent
//! definitions stay main's — pm resolves them from main, so a harness's
//! projected copy comes from main's projection, and the feature gets no
//! canonical copy at all. Runs when a feature is created and on an explicit
//! `pm harness pull` — never as a side effect of another command, so a live
//! feature changes only when someone asks. Copy-only — nothing in the
//! feature is ever deleted, so a skill removed on the branch is copied back
//! from main while main has it — and a file the feature's branch tracks
//! in git is never written: its content is the branch's, and reaches or
//! leaves main by merge.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::fs_utils::{copy_dir_recursive, sync_file, sync_tree_except};
use crate::git;
use crate::harness::ProjectionScope;
use crate::state::paths;

use super::skills::{self, CANONICAL_DIR};

/// The canonical store's subdirs a feature carries its own copy of,
/// whatever its harnesses. The feature's copy is authoritative for its
/// projection; every other projected subdir (agent definitions) stays
/// main-sourced, matching where pm itself resolves definitions from.
const CANONICAL_SUBDIRS: &[&str] = &["skills"];

/// Called during `feat new` / `feat adopt` / `feat review`.
pub fn seed_feature_assets(project_root: &Path, feature_worktree: &Path) -> Result<()> {
    sync_feature(project_root, feature_worktree, false).map(|_| ())
}

/// `pm harness pull`: seed an existing feature again. Returns the files
/// written (or, with `dry_run`, that would be), relative to the worktree.
pub fn pull(project_root: &Path, feature_name: &str, dry_run: bool) -> Result<Vec<PathBuf>> {
    super::feat_common::require_feature(project_root, feature_name)?;
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
    for sub in CANONICAL_SUBDIRS {
        let rel = Path::new(CANONICAL_DIR).join(sub);
        let files = sync_untracked(&main, feature_worktree, &rel, &HashSet::new(), dry_run)?;
        written.extend(files.into_iter().map(|f| rel.join(f)));
    }
    // A dry run projects from what the canonical store would hold.
    let preview = if dry_run {
        Some(preview_canonical(&main, feature_worktree)?)
    } else {
        None
    };
    let canonical = match &preview {
        Some(dir) => dir.path().to_path_buf(),
        None => feature_worktree.join(CANONICAL_DIR),
    };
    for harness in &harnesses {
        let cfg = Path::new(harness.config_dir());
        for sub in harness.projected_dirs() {
            let rel = cfg.join(sub);
            if !CANONICAL_SUBDIRS.contains(sub) {
                let files =
                    sync_untracked(&main, feature_worktree, &rel, &HashSet::new(), dry_run)?;
                written.extend(files.into_iter().map(|f| rel.join(f)));
                continue;
            }
            // Main's projected copy fills in only what the feature's own
            // store lacks (a hand-written harness-only entry), so the
            // projection below never fights it.
            let own = entry_names(&canonical.join(sub))?;
            let files = sync_untracked(&main, feature_worktree, &rel, &own, dry_run)?;
            written.extend(files.into_iter().map(|f| rel.join(f)));
            let scope = ProjectionScope {
                subdirs: Some(std::slice::from_ref(sub)),
                keep: tracked_under(feature_worktree, &rel)?
                    .into_iter()
                    .map(|f| Path::new(sub).join(f))
                    .collect(),
            };
            let projection =
                harness.project_assets(&canonical, &feature_worktree.join(cfg), &scope, dry_run)?;
            written.extend(projection.written.into_iter().map(|f| cfg.join(f)));
        }
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

/// A temporary copy of the feature's [`CANONICAL_SUBDIRS`] with main's
/// untracked files synced in, as a real run leaves them.
fn preview_canonical(main: &Path, feature_worktree: &Path) -> Result<tempfile::TempDir> {
    let preview = tempfile::tempdir()?;
    for sub in CANONICAL_SUBDIRS {
        let rel = Path::new(CANONICAL_DIR).join(sub);
        let own = feature_worktree.join(&rel);
        if own.is_dir() {
            copy_dir_recursive(&own, &preview.path().join(sub))?;
        }
        let src = main.join(&rel);
        if src.is_dir() {
            let keep = tracked_under(feature_worktree, &rel)?;
            sync_tree_except(&src, &preview.path().join(sub), &keep, false)?;
        }
    }
    Ok(preview)
}

/// The names of `dir`'s immediate entries; empty when it doesn't exist.
fn entry_names(dir: &Path) -> Result<HashSet<PathBuf>> {
    if !dir.is_dir() {
        return Ok(HashSet::new());
    }
    std::fs::read_dir(dir)?
        .map(|entry| Ok(PathBuf::from(entry?.file_name())))
        .collect()
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
/// worktree's branch tracks and the paths in `skip` (relative to `rel`).
/// Returns the files written (or, with `dry_run`, that would be), relative
/// to `rel`; a `rel` main doesn't have is a no-op.
fn sync_untracked(
    main: &Path,
    worktree: &Path,
    rel: &Path,
    skip: &HashSet<PathBuf>,
    dry_run: bool,
) -> Result<Vec<PathBuf>> {
    let src = main.join(rel);
    if !src.is_dir() {
        return Ok(Vec::new());
    }
    let mut keep = tracked_under(worktree, rel)?;
    keep.extend(skip.iter().cloned());
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
            (".agents/skills/pm/SKILL.md", "# canonical skill"),
            (".claude/agents/reviewer.md", "# projected"),
            (".claude/skills/pm/SKILL.md", "# canonical skill"),
        ] {
            assert_eq!(read(feature_wt.join(rel)), expected, "{rel}");
        }
        // Definitions resolve from main only, so the feature gets no
        // canonical copy to drift from them.
        assert!(!feature_wt.join(".agents/agents").exists());
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

        let expected: Vec<PathBuf> = [
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
            !feature_wt.join(".claude/agents/custom.md").exists(),
            "dry-run wrote"
        );

        assert_eq!(pull(&project, "login", false).unwrap(), expected);
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
        assert!(pull(&project, "login", false).unwrap().is_empty());

        let err = pull(&project, "nonexistent", false).unwrap_err();
        assert!(matches!(err, PmError::FeatureNotFound(_)));

        // A registered feature whose worktree is gone is not recreated.
        std::fs::remove_dir_all(&feature_wt).unwrap();
        write(&main.join(".agents/skills/later"), "SKILL.md", "later");
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
        let tracked = [".agents/skills/custom/SKILL.md", ".claude/settings.json"];

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
        let expected = vec![
            PathBuf::from(".agents/skills/custom/notes.md"),
            PathBuf::from(".claude/skills/custom/notes.md"),
        ];
        assert_eq!(pull(&project, "login", true).unwrap(), expected);
        assert_eq!(pull(&project, "login", false).unwrap(), expected);
        assert_kept("pull");
    }

    #[test]
    fn feature_projects_its_own_skills_over_mains() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let main = paths::main_worktree(&project);
        let feature_wt = project.join("login");

        write(&main.join(".agents/skills/custom"), "SKILL.md", "main's");
        write(&main.join(".claude/skills/custom"), "SKILL.md", "main's");
        write(
            &main.join(".claude/skills/hand"),
            "SKILL.md",
            "harness-only",
        );
        write(
            &feature_wt.join(".agents/skills/custom"),
            "SKILL.md",
            "branch's",
        );
        write(
            &feature_wt.join(".agents/skills/pinned"),
            "SKILL.md",
            "canonical",
        );
        write(
            &feature_wt.join(".claude/skills/pinned"),
            "SKILL.md",
            "tracked",
        );
        for rel in [
            ".agents/skills/custom/SKILL.md",
            ".claude/skills/pinned/SKILL.md",
        ] {
            git::stage_file(&feature_wt, rel).unwrap();
        }
        git::commit(&feature_wt, "edit skills").unwrap();

        seed_feature_assets(&project, &feature_wt).unwrap();
        let projected = feature_wt.join(".claude/skills");
        assert_eq!(read(projected.join("custom/SKILL.md")), "branch's");
        assert_eq!(read(projected.join("hand/SKILL.md")), "harness-only");
        assert_eq!(read(projected.join("pinned/SKILL.md")), "tracked");
        assert!(pull(&project, "login", false).unwrap().is_empty());

        write(
            &feature_wt.join(".agents/skills/custom"),
            "SKILL.md",
            "edited",
        );
        let expected = vec![PathBuf::from(".claude/skills/custom/SKILL.md")];
        assert_eq!(pull(&project, "login", true).unwrap(), expected);
        assert_eq!(read(projected.join("custom/SKILL.md")), "branch's");
        assert_eq!(pull(&project, "login", false).unwrap(), expected);
        assert_eq!(read(projected.join("custom/SKILL.md")), "edited");
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
    }
}
