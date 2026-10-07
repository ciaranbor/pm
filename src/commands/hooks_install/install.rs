//! Writing pm's entries into every supported harness's user-level hooks
//! file and its plugin files, then stripping the project-level copies.

use std::path::Path;

use serde_json::Value;

use super::check::{install_location, stale_plugin_files};
use super::entries::pm_entries;
use super::project::strip_project;
use super::settings::{load_settings, upsert_hook, write_settings};
use crate::error::Result;
use crate::fs_utils::write_atomic;
use crate::harness::Harness;
use crate::state::paths;

/// Install pm hooks into the user-level file of every supported harness
/// and, when inside a project, strip pm's entries from its project-level
/// files. Returns a human-readable status, one line per file changed.
pub fn install(project_root: Option<&Path>) -> Result<String> {
    let home = paths::home_dir()?;
    let lines = install_in(&home, project_root, false)?;
    if lines.is_empty() {
        let files: Vec<String> = Harness::SUPPORTED
            .iter()
            .filter_map(|h| install_location(*h, &home))
            .map(|path| path.display().to_string())
            .collect();
        return Ok(format!(
            "pm hooks already installed in {}",
            files.join(", ")
        ));
    }
    Ok(lines.join("\n"))
}

/// Dry-run variant of [`install`]: one `Would …` line per file that would
/// change; empty when everything is up to date.
pub fn install_dry_run(project_root: Option<&Path>) -> Result<Vec<String>> {
    install_in(&paths::home_dir()?, project_root, true)
}

/// [`install`] against an explicit `home`, for tests that must not share
/// the per-binary test home. Returns one line per file changed (or, with
/// `dry_run`, per file that would change).
pub(crate) fn install_in(
    home: &Path,
    project_root: Option<&Path>,
    dry_run: bool,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    let verb = if dry_run {
        "Would install"
    } else {
        "Installed"
    };
    for harness in Harness::SUPPORTED {
        if let Some(user_file) = harness.user_settings_file(home)
            && install_global(*harness, &user_file, dry_run)?
        {
            lines.push(format!("{verb} pm hooks in {}", user_file.display()));
        }
        if install_plugin(*harness, home, dry_run)?
            && let Some(dir) = install_location(*harness, home)
        {
            lines.push(format!("{verb} pm plugin in {}", dir.display()));
        }
    }
    if let Some(root) = project_root {
        for path in strip_project(root, dry_run)? {
            lines.push(format!(
                "{} pm hooks from {}",
                if dry_run { "Would remove" } else { "Removed" },
                path.strip_prefix(root).unwrap_or(&path).display()
            ));
        }
    }
    Ok(lines)
}

/// Upsert every pm entry of `harness` into its user-level file. Returns
/// whether the file changed (or would).
fn install_global(harness: Harness, user_file: &Path, dry_run: bool) -> Result<bool> {
    let mut root =
        load_settings(user_file)?.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let mut changed = false;
    for (event, markers, entry) in pm_entries(harness) {
        changed |= upsert_hook(&mut root, event, markers, entry)?;
    }
    if !changed {
        return Ok(false);
    }
    if !dry_run {
        write_settings(user_file, &root)?;
    }
    Ok(true)
}

/// Write the plugin files that are missing or out of date. Returns whether
/// any was (or would be).
fn install_plugin(harness: Harness, home: &Path, dry_run: bool) -> Result<bool> {
    let stale = stale_plugin_files(harness, home);
    if dry_run {
        return Ok(!stale.is_empty());
    }
    for (path, content) in harness.plugin_files(home) {
        if stale.contains(&path) {
            write_atomic(&path, content.as_bytes())?;
        }
    }
    Ok(!stale.is_empty())
}

#[cfg(test)]
mod tests;
