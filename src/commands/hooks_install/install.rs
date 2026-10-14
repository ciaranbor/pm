//! Writing pm's entries into every supported harness's user-level hooks
//! file and its plugin files.

use std::path::Path;

use serde_json::Value;

use super::check::{install_location, stale_plugin_files};
use super::entries::pm_entries;
use super::settings::{
    load_settings, pm_hook_positions, upsert_hook, user_hooks_root, write_settings,
};
use crate::error::Result;
use crate::fs_utils::write_atomic;
use crate::harness::Harness;
use crate::state::paths;
use crate::state::project::{GlobalConfig, HarnessConfig};

/// Install pm hooks into the user-level file of every supported harness,
/// asking each harness afresh whether it trusts them. Returns a
/// human-readable status: one line per file changed or harness that
/// trusted pm's hooks, then any warnings.
pub fn install() -> Result<String> {
    let home = paths::home_dir()?;
    let lines = install_with(&home, false, true)?.lines();
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
pub fn install_dry_run() -> Result<Vec<String>> {
    install_in(&paths::home_dir()?, true)
}

/// [`install`] against an explicit `home`, asking a harness about trust
/// only when its file changed or some hook has never been trusted. Returns
/// one line per file changed (or, with `dry_run`, per file that would
/// change) or harness that trusted pm's hooks, then any warnings.
pub(crate) fn install_in(home: &Path, dry_run: bool) -> Result<Vec<String>> {
    Ok(install_with(home, dry_run, false)?.lines())
}

/// [`install_in`] for a command that reports nothing about the install
/// itself: only why a harness could not trust pm's hooks, on stderr.
pub(crate) fn install_quietly(home: &Path) -> Result<()> {
    for warning in install_with(home, false, false)?.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

/// What an install did: one line per file changed or harness that
/// trusted pm's hooks, and why a harness could not.
struct Installed {
    lines: Vec<String>,
    warnings: Vec<String>,
}

impl Installed {
    fn lines(self) -> Vec<String> {
        let warnings = self.warnings.into_iter().map(|w| format!("warning: {w}"));
        self.lines.into_iter().chain(warnings).collect()
    }
}

/// Record each harness's trust in pm's hooks as installed under `home`,
/// where the user consented to pm doing so; one line per harness that
/// trusted some. `recheck` asks the harness even when every hook has a
/// trust entry, which may be outdated.
pub fn trust_in(home: &Path, recheck: bool) -> Result<Vec<String>> {
    let config = GlobalConfig::load_or_default().harness;
    let mut lines = Vec::new();
    for harness in Harness::SUPPORTED {
        lines.extend(trust_one(*harness, &config, home, recheck)?);
    }
    Ok(lines)
}

fn trust_one(
    harness: Harness,
    config: &HarnessConfig,
    home: &Path,
    recheck: bool,
) -> Result<Option<String>> {
    let Some(root) = user_hooks_root(harness, home)? else {
        return Ok(None);
    };
    harness.trust_pm_hooks(config, home, &pm_hook_positions(harness, &root), recheck)
}

fn install_with(home: &Path, dry_run: bool, recheck: bool) -> Result<Installed> {
    let config = GlobalConfig::load_or_default().harness;
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    let verb = if dry_run {
        "Would install"
    } else {
        "Installed"
    };
    for harness in Harness::SUPPORTED {
        if let Some(user_file) = harness.user_settings_file(home) {
            let changed = install_global(*harness, &user_file, dry_run)?;
            if changed {
                lines.push(format!("{verb} pm hooks in {}", user_file.display()));
            }
            if !dry_run {
                match trust_one(*harness, &config, home, recheck || changed) {
                    Ok(line) => lines.extend(line),
                    Err(e) => warnings.push(e.to_string()),
                }
            }
        }
        if install_plugin(*harness, home, dry_run)?
            && let Some(dir) = install_location(*harness, home)
        {
            lines.push(format!("{verb} pm plugin in {}", dir.display()));
        }
    }
    Ok(Installed { lines, warnings })
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
