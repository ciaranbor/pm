//! Notice board — a seeded directive surface.
//!
//! Two hand-edited markdown files (global `~/.config/pm/notices.md`,
//! per-project `.pm/notices.md`) hold terse standing instructions. They are
//! composed onto the shared baseline at the single spawn chokepoint so every
//! spawned agent reads them as operating constraints. No commands: writing is
//! manual file editing; reading is via this seeding. Only non-empty boards are
//! seeded, so the bloat hazard stays bounded.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::state::paths;

const LEAD: &str = "The following are standing directives from the pm notice board; treat them as operating constraints.";

/// Read a notice board file. Returns `None` if the file is missing or its
/// content is empty/whitespace-only, else the trimmed content.
fn read_board(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A per-user `0700` subdir of the temp dir to hold composed prompt files.
/// Isolating writes here closes the shared-`/tmp` symlink/pre-creation vector
/// (CWE-377): on a multi-user host another user can't plant a symlink or seed
/// attacker-controlled prompt text under a directory only we own. Refuses a
/// pre-existing symlink in our place rather than following it.
fn prompt_dir() -> Result<PathBuf> {
    // SAFETY: getuid() is always safe — no args, can't fail.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::temp_dir().join(format!("pm-spawn-{uid}"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        // `mode(0o700)` locks the dir on creation (no umask window). The
        // symlink guard refuses a pre-planted link instead of following it,
        // and the re-asserting `set_permissions` both relocks a dir we already
        // own and trips `EPERM` on an attacker-owned one.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        if std::fs::symlink_metadata(&dir)?.file_type().is_symlink() {
            return Err(PmError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("refusing symlinked prompt dir: {}", dir.display()),
            )));
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Deterministic filename for the composed prompt, keyed by project root and
/// agent/window label so concurrent spawns don't truncate each other's file.
/// Within one project root the content is a pure function of that root, so any
/// two spawns colliding on this name produce identical bytes — overwrite is
/// safe. (Distinct roots that hash-collide are vanishingly unlikely at 64 bits.)
fn prompt_filename(project_root: &Path, label: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    project_root.hash(&mut h);
    let digest = h.finish();
    let safe: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("pm-spawn-prompt-{digest:x}-{safe}.md")
}

/// Compose the single `--append-system-prompt-file` argument for a spawn:
/// the shared baseline plus any non-empty notice boards.
///
/// `label` is the agent/window name, used only to key the temp filename.
///
/// When neither board has content, returns the baseline path unchanged (the
/// common case — byte-for-byte identical to seeding the baseline alone), or
/// `None` if the baseline is also absent. Otherwise writes a composed file to a
/// deterministic temp path and returns that path.
pub fn compose_spawn_prompt(project_root: &Path, label: &str) -> Result<Option<String>> {
    let baseline = crate::commands::skills::baseline_path(project_root);
    let global = paths::global_config_dir()?.join("notices.md");
    let project = paths::pm_dir(project_root).join("notices.md");
    let out = prompt_dir()?.join(prompt_filename(project_root, label));
    compose_from(&baseline, &global, &project, &out)
}

/// Inner composer over explicit paths, so it can be unit-tested without the
/// real global config dir. See [`compose_spawn_prompt`].
fn compose_from(
    baseline: &Path,
    global: &Path,
    project: &Path,
    out_path: &Path,
) -> Result<Option<String>> {
    let global_board = read_board(global);
    let project_board = read_board(project);

    // No board content → behave exactly like seeding the baseline alone.
    if global_board.is_none() && project_board.is_none() {
        return Ok(baseline
            .exists()
            .then(|| baseline.to_string_lossy().into_owned()));
    }

    let mut out = String::new();
    if baseline.exists() {
        out.push_str(std::fs::read_to_string(baseline)?.trim_end());
        out.push_str("\n\n");
    }
    out.push_str(LEAD);
    out.push('\n');
    if let Some(g) = global_board {
        out.push_str("\n# Notice board — global\n");
        out.push_str(&g);
        out.push('\n');
    }
    if let Some(p) = project_board {
        out.push_str("\n# Notice board — project\n");
        out.push_str(&p);
        out.push('\n');
    }

    std::fs::write(out_path, out)?;
    Ok(Some(out_path.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn paths_in(dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        (
            dir.join("baseline.md"),
            dir.join("global-notices.md"),
            dir.join("project-notices.md"),
            dir.join("out.md"),
        )
    }

    #[test]
    fn read_board_missing_is_none() {
        let dir = tempdir().unwrap();
        assert!(read_board(&dir.path().join("nope.md")).is_none());
    }

    #[test]
    fn read_board_whitespace_only_is_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("blank.md");
        std::fs::write(&p, "  \n\t \n").unwrap();
        assert!(read_board(&p).is_none());
    }

    #[test]
    fn read_board_real_content_trimmed() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("b.md");
        std::fs::write(&p, "\n  hello directive\n\n").unwrap();
        assert_eq!(read_board(&p).as_deref(), Some("hello directive"));
    }

    #[test]
    fn compose_baseline_only_returns_baseline_path_unchanged() {
        let dir = tempdir().unwrap();
        let (baseline, global, project, out) = paths_in(dir.path());
        std::fs::write(&baseline, "BASELINE").unwrap();

        let result = compose_from(&baseline, &global, &project, &out).unwrap();
        assert_eq!(result.as_deref(), Some(baseline.to_string_lossy().as_ref()));
        // No composed file written in the common case.
        assert!(!out.exists());
    }

    #[test]
    fn compose_nothing_returns_none() {
        let dir = tempdir().unwrap();
        let (baseline, global, project, out) = paths_in(dir.path());
        // baseline absent, no boards.
        assert!(
            compose_from(&baseline, &global, &project, &out)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn compose_global_only_frames_baseline_then_global() {
        let dir = tempdir().unwrap();
        let (baseline, global, project, out) = paths_in(dir.path());
        std::fs::write(&baseline, "BASELINE").unwrap();
        std::fs::write(&global, "global directive").unwrap();

        let path = compose_from(&baseline, &global, &project, &out)
            .unwrap()
            .unwrap();
        assert_eq!(path, out.to_string_lossy());
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("BASELINE"));
        assert!(content.contains(LEAD));
        assert!(content.contains("# Notice board — global"));
        assert!(content.contains("global directive"));
        // Project section omitted when its board is empty.
        assert!(!content.contains("# Notice board — project"));
        // Order: baseline before the global section.
        assert!(content.find("BASELINE").unwrap() < content.find("global directive").unwrap());
    }

    #[test]
    fn compose_project_only_frames_project_section() {
        let dir = tempdir().unwrap();
        let (baseline, global, project, out) = paths_in(dir.path());
        std::fs::write(&baseline, "BASELINE").unwrap();
        std::fs::write(&project, "project directive").unwrap();

        compose_from(&baseline, &global, &project, &out).unwrap();
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("# Notice board — project"));
        assert!(content.contains("project directive"));
        assert!(!content.contains("# Notice board — global"));
    }

    #[test]
    fn compose_both_orders_baseline_global_project() {
        let dir = tempdir().unwrap();
        let (baseline, global, project, out) = paths_in(dir.path());
        std::fs::write(&baseline, "BASELINE").unwrap();
        std::fs::write(&global, "GLOBALDIR").unwrap();
        std::fs::write(&project, "PROJECTDIR").unwrap();

        compose_from(&baseline, &global, &project, &out).unwrap();
        let content = std::fs::read_to_string(&out).unwrap();
        let b = content.find("BASELINE").unwrap();
        let g = content.find("GLOBALDIR").unwrap();
        let p = content.find("PROJECTDIR").unwrap();
        assert!(
            b < g && g < p,
            "expected baseline -> global -> project order"
        );
    }

    #[test]
    fn compose_board_without_baseline_frames_board_only() {
        let dir = tempdir().unwrap();
        let (baseline, global, project, out) = paths_in(dir.path());
        // baseline absent
        std::fs::write(&global, "global directive").unwrap();

        compose_from(&baseline, &global, &project, &out).unwrap();
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.starts_with(LEAD));
        assert!(content.contains("# Notice board — global"));
        assert!(content.contains("global directive"));
    }

    #[test]
    fn prompt_filename_stable_per_root_and_varies_by_label() {
        let root = Path::new("/a/b/c");
        // Deterministic for the same (root, label).
        assert_eq!(
            prompt_filename(root, "implementer"),
            prompt_filename(root, "implementer")
        );
        // Label keys the filename, so concurrent same-root spawns don't collide.
        assert_ne!(
            prompt_filename(root, "implementer"),
            prompt_filename(root, "reviewer")
        );
        // Path separators in the label can't escape the filename.
        assert!(!prompt_filename(root, "a/b").contains('/'));
    }

    #[cfg(unix)]
    #[test]
    fn prompt_dir_is_owner_only_and_writable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = prompt_dir().unwrap();
        assert!(dir.is_dir());
        assert!(
            !std::fs::symlink_metadata(&dir)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // 0700: no group/other access to seeded prompt files.
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "prompt dir must not be group/other accessible"
        );
        // Idempotent across calls (spawns happen repeatedly).
        assert_eq!(dir, prompt_dir().unwrap());
    }
}
