//! Keeps the files pm writes into a project repo's worktrees out of git
//! without committing anything to the repo: their paths go in pm's block of
//! the clone's `info/exclude` ([`git::write_excluded_paths`]), which every
//! worktree shares.
//!
//! Covered: main's canonical store (`.agents/{agents,skills}` and a project
//! baseline) and harness dirs, which features are seeded from, and in every
//! worktree what pm projected or seeded there (`pm_files` says exactly
//! what). A skill a feature adds to its own canonical store is not covered,
//! so its branch can commit it; its projection is. The block names files
//! one by one, since a directory pattern would also hide such a skill, and
//! never one any worktree tracks: a team that commits shared definitions
//! keeps them. It is recomputed whenever pm projects or seeds, under a lock
//! beside the exclude file so concurrent seeds don't drop each other's
//! lines.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::git;
use crate::harness::Harness;
use crate::state::paths;

use super::skills::{BASELINE_FILE, CANONICAL_DIR};

/// Rewrite the repo's exclude block to list every file pm wrote and the
/// repo doesn't track. Returns how many it lists that it did not before.
pub fn sync(project_root: &Path) -> Result<usize> {
    let main = paths::main_worktree(project_root);
    if !git::is_git_repo(&main) {
        return Ok(0);
    }
    let exclude = git::info_exclude_path(&main)?;
    let _lock = lock(&exclude)?;
    let listed = git::excluded_paths(&exclude)?;
    let files = pm_files(&main, &listed, &[])?;
    git::write_excluded_paths(&exclude, &files)?;
    Ok(files.difference(&listed).count())
}

/// Held for a sync's read-compute-write, so one never writes a block
/// computed before another's seed wrote its files.
fn lock(exclude: &Path) -> Result<std::fs::File> {
    let dir = exclude.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("pm-exclude.lock"))?;
    file.lock()?;
    Ok(file)
}

/// Files pm wrote, or is about to write at `pending` (relative to the
/// worktree root), that the repo's exclude block does not yet list.
pub fn missing(project_root: &Path, pending: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let main = paths::main_worktree(project_root);
    if !git::is_git_repo(&main) {
        return Ok(Vec::new());
    }
    let listed = git::excluded_paths(&git::info_exclude_path(&main)?)?;
    Ok(pm_files(&main, &listed, pending)?
        .into_iter()
        .filter(|f| !listed.contains(f))
        .collect())
}

/// Every file pm writes that is on disk in some worktree of `main`'s repo,
/// plus `pending`, minus what the repo tracks. In main, every file under the
/// canonical store and the harnesses' projected dirs counts: seeding copies
/// all of main's untracked files there into each feature, where a visible
/// copy would block `pm feat merge` and invite a commit. In a feature, a
/// projected-dir entry counts only when pm put it there (its source is in
/// main's or the feature's canonical store, or main has it), so one written
/// there by hand stays visible. A `listed` path stays while any worktree
/// still holds it: a feature's seeded copy of a skill main has since
/// deleted is still pm's.
fn pm_files(
    main: &Path,
    listed: &BTreeSet<PathBuf>,
    pending: &[PathBuf],
) -> Result<BTreeSet<PathBuf>> {
    let worktrees: Vec<PathBuf> = git::list_worktrees(main)?
        .into_iter()
        .map(PathBuf::from)
        .filter(|w| w.is_dir())
        .collect();
    let mut prefixes = vec![CANONICAL_DIR.to_string()];
    prefixes.extend(
        Harness::SUPPORTED
            .iter()
            .map(|h| h.config_dir().to_string()),
    );
    let prefixes: Vec<&str> = prefixes.iter().map(String::as_str).collect();
    let mut tracked = BTreeSet::new();
    for worktree in &worktrees {
        tracked.extend(git::tracked_under(worktree, &prefixes)?);
    }

    let canonical = Path::new(CANONICAL_DIR);
    let mut found = BTreeSet::new();
    for rel in [
        canonical.join("agents"),
        canonical.join("skills"),
        canonical.join(BASELINE_FILE),
    ] {
        collect(main, &rel, &mut found)?;
    }
    found.extend(
        listed
            .iter()
            .filter(|p| {
                worktrees
                    .iter()
                    .any(|w| std::fs::symlink_metadata(w.join(p)).is_ok())
            })
            .cloned(),
    );

    let main_id = main.canonicalize().unwrap_or_else(|_| main.to_path_buf());
    for h in Harness::SUPPORTED {
        let cfg = Path::new(h.config_dir());
        for worktree in &worktrees {
            let is_main = worktree.canonicalize().is_ok_and(|w| w == main_id);
            for sub in h.projected_dirs() {
                let dir = cfg.join(sub);
                for name in entry_names(&worktree.join(&dir))? {
                    let source = canonical.join(sub).join(&name);
                    if is_main
                        || main.join(&source).exists()
                        || worktree.join(&source).exists()
                        || main.join(&dir).join(&name).exists()
                    {
                        collect(worktree, &dir.join(name), &mut found)?;
                    }
                }
            }
            for file in h.seeded_files() {
                collect(worktree, &cfg.join(file), &mut found)?;
            }
        }
    }
    found.extend(pending.iter().cloned());
    Ok(found.difference(&tracked).cloned().collect())
}

/// The entries directly under `dir`; none when it is absent.
fn entry_names(dir: &Path) -> Result<Vec<std::ffi::OsString>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| Ok(e?.file_name()))
            .collect::<std::io::Result<_>>()
            .map_err(Into::into),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// Add `rel` (relative to `root`) to `out` if it is a file, or every file
/// under it if it is a directory.
fn collect(root: &Path, rel: &Path, out: &mut BTreeSet<PathBuf>) -> Result<()> {
    let meta = match std::fs::symlink_metadata(root.join(rel)) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_dir() {
        out.insert(rel.to_path_buf());
        return Ok(());
    }
    for entry in std::fs::read_dir(root.join(rel))? {
        collect(root, &rel.join(entry?.file_name()), out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::testing::TestServer;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn seeded_and_projected_files_leave_git_status_but_tracked_and_feature_owned_ones_stay() {
        let server = TestServer::new();
        let dir = tempfile::tempdir().unwrap();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        let main = paths::main_worktree(&project);
        write(&main.join(".agents/skills/mine/SKILL.md"), "mine");
        write(&main.join(".agents/skills/shared/SKILL.md"), "shared");
        write(&main.join(".claude/settings.json"), "{}");
        git::stage_file(&main, ".agents/skills/shared/SKILL.md").unwrap();
        git::commit(&main, "team skill").unwrap();
        crate::commands::skills::project_assets(&project, false).unwrap();

        let feature = project.join("feat");
        git::create_branch_from(&main, "feat", "main").unwrap();
        git::add_worktree(&main, &feature, "feat").unwrap();
        write(&feature.join(".agents/skills/new/SKILL.md"), "new");
        crate::commands::seed::seed_feature_assets(&project, &feature).unwrap();

        assert!(git::untracked_files(&main).unwrap().is_empty());
        assert_eq!(
            git::untracked_files(&feature).unwrap(),
            vec![".agents/skills/new/SKILL.md".to_string()]
        );
        let listed = git::excluded_paths(&git::info_exclude_path(&main).unwrap()).unwrap();
        assert!(listed.contains(Path::new(".claude/skills/new/SKILL.md")));
        assert!(!listed.contains(Path::new(".agents/skills/shared/SKILL.md")));
        assert!(missing(&project, &[]).unwrap().is_empty());
    }

    #[test]
    fn doctor_reports_unlisted_files_and_its_fix_hides_them() {
        let server = TestServer::new();
        let dir = tempfile::tempdir().unwrap();
        let (project, projects_dir, _) = server.setup_project_no_tmux(dir.path());
        let main = paths::main_worktree(&project);
        write(&main.join(".agents/skills/a/SKILL.md"), "a");
        let reported = || {
            crate::commands::doctor::doctor(&project, &projects_dir, false, server.name())
                .unwrap()
                .lines()
                .into_iter()
                .any(|l| l.contains("pm wrote show as untracked in git"))
        };

        assert!(reported());
        crate::commands::doctor::doctor(&project, &projects_dir, true, server.name()).unwrap();

        assert!(!reported());
        assert!(git::untracked_files(&main).unwrap().is_empty());
    }

    #[test]
    fn main_files_seeded_into_a_feature_are_hidden_but_a_features_hand_written_one_is_not() {
        let server = TestServer::new();
        let dir = tempfile::tempdir().unwrap();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        let main = paths::main_worktree(&project);
        write(&main.join(".claude/agents/hand.md"), "hand");
        write(&main.join(".agents/skills/team/SKILL.md"), "team");
        git::stage_file(&main, ".agents/skills/team/SKILL.md").unwrap();
        git::commit(&main, "team skill").unwrap();
        write(&main.join(".agents/skills/team/notes.md"), "notes");
        crate::commands::skills::project_assets(&project, false).unwrap();
        let feature = project.join("feat");
        git::create_branch_from(&main, "feat", "main").unwrap();
        git::add_worktree(&main, &feature, "feat").unwrap();
        write(&feature.join(".claude/agents/own.md"), "own");

        crate::commands::seed::seed_feature_assets(&project, &feature).unwrap();

        assert!(feature.join(".claude/agents/hand.md").exists());
        assert!(feature.join(".agents/skills/team/notes.md").exists());
        assert!(git::untracked_files(&main).unwrap().is_empty());
        assert_eq!(
            git::untracked_files(&feature).unwrap(),
            vec![".claude/agents/own.md".to_string()]
        );
    }

    #[test]
    fn a_deleted_file_leaves_the_block() {
        let server = TestServer::new();
        let dir = tempfile::tempdir().unwrap();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        let main = paths::main_worktree(&project);
        write(&main.join(".agents/skills/a/SKILL.md"), "a");
        crate::commands::skills::project_assets(&project, false).unwrap();

        fs::remove_dir_all(main.join(".agents/skills/a")).unwrap();
        fs::remove_dir_all(main.join(".claude/skills/a")).unwrap();
        assert_eq!(sync(&project).unwrap(), 0);
        let listed = git::excluded_paths(&git::info_exclude_path(&main).unwrap()).unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn a_features_seeded_copy_stays_hidden_after_main_deletes_the_skill() {
        let server = TestServer::new();
        let dir = tempfile::tempdir().unwrap();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        let main = paths::main_worktree(&project);
        write(&main.join(".agents/skills/two/SKILL.md"), "two");
        crate::commands::skills::project_assets(&project, false).unwrap();
        let feature = project.join("feat");
        git::create_branch_from(&main, "feat", "main").unwrap();
        git::add_worktree(&main, &feature, "feat").unwrap();
        crate::commands::seed::seed_feature_assets(&project, &feature).unwrap();

        fs::remove_dir_all(main.join(".agents/skills/two")).unwrap();
        sync(&project).unwrap();

        assert!(git::untracked_files(&feature).unwrap().is_empty());
    }

    #[test]
    fn a_dry_run_counts_the_projection_it_would_write() {
        let server = TestServer::new();
        let dir = tempfile::tempdir().unwrap();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        write(
            &paths::main_worktree(&project).join(".agents/skills/a/SKILL.md"),
            "a",
        );
        let count = |lines: Vec<String>| lines.into_iter().find(|l| l.contains("exclude"));

        let dry = count(crate::commands::skills::project_assets(&project, true).unwrap());
        let real = count(crate::commands::skills::project_assets(&project, false).unwrap());

        assert_eq!(
            dry.unwrap().replace("Would exclude", "Excluded"),
            real.unwrap()
        );
    }
}
