//! What `pm doctor` says of pm's global files: what the move left at the
//! legacy location and why, `pm serve`'s secrets readable by others, and
//! tmux configs naming the legacy dir, which pm never edits.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{FAILED, LEGACY_MACHINE_DIRS, Outcome, failed_marker, needed, outcome_notes, plan};
use crate::state::dirs::{Dirs, legacy_dir};
use crate::state::paths;
use crate::state::serve_files::ServeFiles;

/// Doctor's warnings about pm's global files.
pub fn warnings() -> Vec<String> {
    let Ok(home) = paths::home_dir() else {
        return Vec::new();
    };
    let dirs = paths::dirs_under(&home);
    let legacy = legacy_dir(&home, &dirs);
    let mut out = warnings_in(&legacy, &dirs);
    out.extend(tmux_conf_findings(&home, &legacy, &dirs));
    out
}

pub(super) fn warnings_in(legacy: &Path, dirs: &Dirs) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(error) = std::fs::read_to_string(failed_marker(dirs)) {
        out.push(format!(
            "pm files — moving them from {} into the XDG dirs failed ({error}); \
             `pm upgrade` tries again",
            legacy.display()
        ));
    }
    if needed(legacy, dirs) {
        let left = left_at(legacy, dirs);
        if !left.is_empty() {
            out.push(format!(
                "pm files — {} still holds {}; `pm upgrade` moves what it can",
                legacy.display(),
                left.join(", ")
            ));
        }
        let (_, outcome) = plan(legacy, dirs);
        out.extend(outcome_notes(
            &Outcome {
                moved: Vec::new(),
                ..outcome
            },
            legacy,
            dirs,
        ));
    }
    out.extend(permission_warnings(&ServeFiles::in_dirs(dirs)));
    out
}

/// The names still at `legacy`.
fn left_at(legacy: &Path, dirs: &Dirs) -> Vec<String> {
    let mut names: Vec<String> = if *legacy != dirs.config {
        std::fs::read_dir(legacy)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    } else {
        LEGACY_MACHINE_DIRS
            .iter()
            .filter(|d| legacy.join(d).exists())
            .map(|d| d.to_string())
            .collect()
    };
    names.retain(|n| n != FAILED);
    names.sort();
    names
}

/// `pm serve`'s dir open to others, or a secret not the user's alone.
pub(super) fn permission_warnings(files: &ServeFiles) -> Vec<String> {
    let mode = |p: &Path| {
        std::fs::metadata(p)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
    };
    let mut out = Vec::new();
    if let Some(m) = mode(&files.dir).filter(|m| m & 0o077 != 0) {
        out.push(format!(
            "pm files — {} is {m:o}, open to other users; `chmod 700` it",
            files.dir.display()
        ));
    }
    for secret in [files.devices(), files.key()] {
        if let Some(m) = mode(&secret).filter(|m| *m != 0o600) {
            out.push(format!(
                "pm files — {} is {m:o}; `chmod 600` it, as it holds secrets",
                secret.display()
            ));
        }
    }
    out
}

/// A line for each tmux config under `home` that names a dir the move
/// emptied, saying what to change it to. pm never edits them.
pub fn tmux_conf_findings(home: &Path, legacy: &Path, dirs: &Dirs) -> Vec<String> {
    let moved: Vec<PathBuf> = if *legacy != dirs.config {
        vec![legacy.to_path_buf()]
    } else {
        LEGACY_MACHINE_DIRS.iter().map(|d| legacy.join(d)).collect()
    };
    let config_home = dirs.config.parent().unwrap_or(home);
    let confs = [home.join(".tmux.conf"), config_home.join("tmux/tmux.conf")];
    let mut out = Vec::new();
    for conf in confs {
        let Ok(text) = std::fs::read_to_string(&conf) else {
            continue;
        };
        for dir in &moved {
            let spellings = spellings(home, dir);
            if spellings.iter().any(|s| text.contains(s.as_str())) {
                out.push(format!(
                    "tmux config — {} names {}, which pm no longer uses; pm's files are now \
                     under {} (config), {} (state) and {} (runtime)",
                    conf.display(),
                    spellings[0],
                    dirs.config.display(),
                    dirs.state.display(),
                    dirs.runtime.display()
                ));
            }
        }
    }
    out
}

/// The ways a config may spell `dir`: absolute, and from `~` and `$HOME`.
fn spellings(home: &Path, dir: &Path) -> Vec<String> {
    let mut out = vec![dir.display().to_string()];
    if let Ok(rel) = dir.strip_prefix(home) {
        out.push(format!("~/{}", rel.display()));
        out.push(format!("$HOME/{}", rel.display()));
    }
    out
}
