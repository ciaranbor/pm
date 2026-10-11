//! `pm harness settings`: compare and move a harness's per-worktree
//! settings files ([`Harness::seeded_files`]) between main and a feature. A
//! harness with none is refused. Diff and merge read the files as JSON —
//! true of every seeded file today; anything else diffs as "content
//! differs" and merges winner-takes-all.

mod diff;
mod list;
mod merge;
mod transfer;

pub use diff::diff;
pub use list::list;
pub use merge::merge;
pub use transfer::{pull, push};

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::harness::Harness;
use crate::state::{paths, scope};

/// The harness's settings files, or why it has none to manage.
pub fn settings_files(harness: Harness) -> Result<&'static [&'static str]> {
    match harness.seeded_files() {
        [] => Err(PmError::Agent(format!(
            "{harness} has no per-feature settings files"
        ))),
        files => Ok(files),
    }
}

/// The directory holding `harness`'s settings files in `scope`'s worktree.
fn settings_dir(project_root: &Path, scope: &str, harness: Harness) -> PathBuf {
    paths::scope_worktree(project_root, scope).join(harness.config_dir())
}

/// A pair of optional file contents (main, feature) for a single settings file.
struct FilePair {
    filename: &'static str,
    main: Option<String>,
    feature: Option<String>,
}

/// Load both sides (main + feature) for each settings file.
fn load_file_pairs(
    project_root: &Path,
    feature_name: &str,
    files: &[&'static str],
    harness: Harness,
) -> Result<Vec<FilePair>> {
    let main_dir = settings_dir(project_root, scope::MAIN, harness);
    let feature_dir = settings_dir(project_root, feature_name, harness);
    let read = |path: PathBuf| -> Result<Option<String>> {
        Ok(if path.exists() {
            Some(std::fs::read_to_string(&path)?)
        } else {
            None
        })
    };
    files
        .iter()
        .map(|&filename| {
            Ok(FilePair {
                filename,
                main: read(main_dir.join(filename))?,
                feature: read(feature_dir.join(filename))?,
            })
        })
        .collect()
}

// ANSI color helpers
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// Helpers the submodules' tests share.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::harness::Harness;
    use std::path::Path;

    pub const CC: Harness = Harness::ClaudeCode;

    pub fn write_json(dir: &Path, filename: &str, content: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(filename), content).unwrap();
    }

    /// Strip ANSI escape sequences for easier assertions.
    pub fn strip_ansi(s: &str) -> String {
        let re = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
        re.replace_all(s, "").to_string()
    }
}
