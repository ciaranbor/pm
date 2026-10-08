//! The one-time move of pm's global files from where an earlier release
//! kept them ([`legacy_dir`]) into the XDG dirs ([`Dirs`]). On macOS the
//! whole config set moves out of `~/Library/Application Support/pm`; on
//! every OS the machine-local dirs move out of the config dir: the probe
//! cache to the cache dir, `pm serve`'s devices, key, log and `state.json`
//! and the registry entries a pull set aside to the state dir.
//!
//! Only the `pm` on `PATH` moves anything: every other pm process (hooks,
//! the tmux watcher, `pm serve`, the user's shell) runs that one, so a build
//! run from elsewhere (`cargo run`, a test, a downloaded binary) moving the
//! files would leave them all reading an emptied legacy dir. Hooks run pm
//! constantly, so [`lazy`] runs before every command but `pm upgrade` and
//! costs a stat or two when nothing is left at the legacy location, and a
//! plan without the lock when only what it won't touch is;
//! `pm upgrade` runs it itself and reports. Runs that act are serialized by
//! a lock in the runtime dir and re-plan under it.
//!
//! Nothing is overwritten. When both config dirs hold config the
//! config set stays put but for registry entries (one file per project)
//! only the legacy dir has, which move, and `pm doctor` names the conflict
//! and each entry registered in both; a machine-local
//! file whose destination exists stays, but for the log, which goes beside
//! the new one as `serve.log.old` (launchd opens the new log before the
//! server it restarts gets to move the old). While a server of an earlier
//! release holds the legacy `serve.lock`, `pm serve`'s files stay with it
//! ([`ServeFiles::global`] keeps using them) until it restarts; while it or
//! an earlier release's tmux watcher holds a legacy lock, so does the config
//! set, which they read the registry from (the new binary uses it in place,
//! [`paths::global_config_dir`]). Both re-execute when their binary is
//! replaced, so this lasts until they notice.
//!
//! A run is all or nothing: each item is renamed into place, or, across
//! filesystems, copied to a temp sibling and renamed; any failure undoes
//! what was done, and sources of copies are removed only once every item
//! has landed. A failed lazy run leaves a marker so later commands don't
//! retry it on every hook; `pm upgrade` retries and `pm doctor` reports it.
//! Runtime files are never moved: legacy locks no one holds and the wake
//! FIFO are deleted.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::state::dirs::{CONFIG_ITEMS, Dirs, legacy_dir};
use crate::state::paths;
use crate::state::serve_files::{self, ServeFiles};

mod doctor;
mod moves;
#[cfg(test)]
mod tests;

pub use doctor::{tmux_conf_findings, warnings};

/// The machine-local dirs an earlier release wrote under the config dir.
pub const LEGACY_MACHINE_DIRS: &[&str] = &[CACHE_DIR, TMUX_DIR, "serve", SET_ASIDE_DIR];
const CACHE_DIR: &str = "cache";
const TMUX_DIR: &str = "tmux";
const SET_ASIDE_DIR: &str = "registry-before-pull";
const REGISTRY_DIR: &str = "projects";

/// Left in the state dir by a lazy run that failed, holding its error.
const FAILED: &str = "xdg-migration-failed";
const LOCK: &str = "xdg-migration.lock";

/// What a run did, or with `dry_run` would do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Legacy-relative paths moved.
    pub moved: Vec<String>,
    /// Both config dirs hold config, so the config set stayed but for
    /// registry entries only the legacy dir had.
    pub conflict: bool,
    /// Legacy-relative registry entries the config dir has too, so kept.
    pub registered_in_both: Vec<String>,
    /// A server of an earlier release holds the legacy lock, so its files
    /// stayed.
    pub serve_deferred: bool,
    /// A process of an earlier release (its server or a tmux watcher) holds
    /// a legacy lock and reads the registry there, so the config set stayed.
    pub config_deferred: bool,
}

/// Whether anything is left at `legacy` to move.
pub fn needed(legacy: &Path, dirs: &Dirs) -> bool {
    if *legacy != dirs.config {
        return std::fs::symlink_metadata(legacy).is_ok();
    }
    LEGACY_MACHINE_DIRS
        .iter()
        .any(|d| std::fs::symlink_metadata(legacy.join(d)).is_ok())
}

/// Move what is left at `legacy` into `dirs`.
pub fn migrate(legacy: &Path, dirs: &Dirs, dry_run: bool) -> Result<Outcome> {
    if !needed(legacy, dirs) {
        return Ok(Outcome::default());
    }
    let (plan, outcome) = plan(legacy, dirs);
    if dry_run || (plan.is_empty() && leftovers(legacy, dirs, outcome.serve_deferred).is_empty()) {
        return Ok(outcome);
    }
    let _lock = lock(dirs)?;
    let (plan, outcome) = self::plan(legacy, dirs);
    moves::execute(&plan)?;
    let serve = ServeFiles::in_dirs(dirs);
    if plan.iter().any(|(_, dst)| dst.starts_with(&serve.dir)) {
        serve.create()?;
        for secret in [serve.devices(), serve.key()] {
            if secret.exists() {
                std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    let cache = legacy.join(CACHE_DIR);
    for path in leftovers(legacy, dirs, outcome.serve_deferred) {
        let _ = if path == cache {
            moves::remove(&path)
        } else if path.is_dir() {
            std::fs::remove_dir(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
    Ok(outcome)
}

/// The moves to make, source then destination, and the outcome they make.
fn plan(legacy: &Path, dirs: &Dirs) -> (Vec<(PathBuf, PathBuf)>, Outcome) {
    let mut plan = Vec::new();
    let mut outcome = Outcome::default();
    let exists = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let add = |src: PathBuf, dst: PathBuf, plan: &mut Vec<(PathBuf, PathBuf)>| {
        if exists(&src) && !exists(&dst) {
            plan.push((src, dst));
        }
    };

    let old = ServeFiles::legacy(legacy);
    if *legacy != dirs.config {
        if old.held() || !held_tmux_locks(legacy).is_empty() {
            outcome.config_deferred = true;
        } else if paths::holds_config(&dirs.config) && paths::holds_config(legacy) {
            outcome.conflict = true;
            for entry in registry_entries(legacy) {
                let dst = dirs
                    .config
                    .join(REGISTRY_DIR)
                    .join(entry.file_name().unwrap());
                if exists(&dst) {
                    let name = entry.strip_prefix(legacy).unwrap_or(&entry);
                    outcome.registered_in_both.push(name.display().to_string());
                } else {
                    add(entry, dst, &mut plan);
                }
            }
        } else {
            for item in CONFIG_ITEMS {
                add(legacy.join(item), dirs.config.join(item), &mut plan);
            }
        }
    }

    let probes = crate::harness::probe::CACHE_FILE;
    add(
        legacy.join(CACHE_DIR).join(probes),
        dirs.cache.join(probes),
        &mut plan,
    );

    if old.held() {
        outcome.serve_deferred = true;
    } else {
        let new = ServeFiles::in_dirs(dirs);
        for name in [serve_files::DEVICES, serve_files::KEY, serve_files::STATE] {
            add(old.dir.join(name), new.dir.join(name), &mut plan);
        }
        let log = (0..)
            .map(|n| match n {
                0 => new.log(),
                1 => new.dir.join("serve.log.old"),
                n => new.dir.join(format!("serve.log.old.{n}")),
            })
            .find(|p| !exists(p))
            .expect("a free name");
        add(old.log(), log, &mut plan);
    }

    add(
        legacy.join(SET_ASIDE_DIR),
        dirs.state
            .join(crate::commands::state_cmd::SET_ASIDE_DIR_NAME),
        &mut plan,
    );

    outcome.moved = plan
        .iter()
        .map(|(src, _)| {
            src.strip_prefix(legacy)
                .unwrap_or(src)
                .display()
                .to_string()
        })
        .collect();
    (plan, outcome)
}

/// What is left to delete at `legacy` once its files moved: the cache,
/// runtime files no one holds (the legacy server's only when it doesn't run),
/// then each legacy dir left empty, in that order.
fn leftovers(legacy: &Path, dirs: &Dirs, serve_deferred: bool) -> Vec<PathBuf> {
    let exists = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let mut out: Vec<PathBuf> = Vec::new();
    let cache = legacy.join(CACHE_DIR);
    if exists(&cache) {
        out.push(cache);
    }
    let held = held_tmux_locks(legacy);
    out.extend(
        lock_files(&legacy.join(TMUX_DIR))
            .into_iter()
            .filter(|p| !held.contains(p)),
    );
    if !serve_deferred {
        let old = ServeFiles::legacy(legacy);
        // Nothing reads the FIFO once the server's lock is free; opening it
        // to test a lock would block.
        out.extend(
            [old.lock(), old.devices_lock(), old.wake()]
                .into_iter()
                .filter(|p| exists(p) && (p.extension().is_none() || !serve_files::held(p))),
        );
    }
    let empty_once = |dir: &Path, gone: &[PathBuf]| {
        std::fs::read_dir(dir)
            .is_ok_and(|entries| entries.flatten().all(|e| gone.contains(&e.path())))
    };
    for dir in LEGACY_MACHINE_DIRS.iter().map(|d| legacy.join(d)) {
        if !out.contains(&dir) && empty_once(&dir, &out) {
            out.push(dir);
        }
    }
    let registry = legacy.join(REGISTRY_DIR);
    if *legacy != dirs.config && empty_once(&registry, &out) {
        out.push(registry);
    }
    if *legacy != dirs.config && empty_once(legacy, &out) {
        out.push(legacy.to_path_buf());
    }
    out
}

/// The project registry entries at `config`, sorted.
fn registry_entries(config: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(config.join(REGISTRY_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml") && p.is_file())
        .collect();
    entries.sort();
    entries
}

fn lock_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "lock"))
        .collect()
}

/// The legacy tmux watcher locks an earlier release's watcher holds.
fn held_tmux_locks(legacy: &Path) -> Vec<PathBuf> {
    lock_files(&legacy.join(TMUX_DIR))
        .into_iter()
        .filter(|p| serve_files::held(p))
        .collect()
}

fn lock(dirs: &Dirs) -> Result<std::fs::File> {
    std::fs::create_dir_all(&dirs.runtime)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dirs.runtime.join(LOCK))?;
    file.lock()?;
    Ok(file)
}

/// Whether this process is the `pm` on `PATH`.
fn is_installed_pm() -> bool {
    std::env::current_exe().is_ok_and(|exe| is_on_path(&exe, std::env::var_os("PATH").as_deref()))
}

/// Whether `exe` is the `pm` that `path` (a `PATH` value) resolves to.
fn is_on_path(exe: &Path, path: Option<&std::ffi::OsStr>) -> bool {
    let on_path = crate::fs_utils::resolve_binary("pm", path);
    on_path.is_some() && exe.canonicalize().ok() == on_path
}

/// The marker a failed lazy run left, holding its error.
fn failed_marker(dirs: &Dirs) -> PathBuf {
    dirs.state.join(FAILED)
}

/// Run before a command: move anything left, saying so on stderr only when
/// something moved or the move failed. Once a run has failed it is left to
/// `pm upgrade`.
pub fn lazy() {
    let Ok(home) = paths::home_dir() else {
        return;
    };
    let dirs = paths::dirs_under(&home);
    let legacy = legacy_dir(&home, &dirs);
    if !needed(&legacy, &dirs) || !is_installed_pm() {
        return;
    }
    if let Some(line) = lazy_in(&legacy, &dirs) {
        eprintln!("{line}");
    }
}

/// [`lazy`]'s run, and what it says.
fn lazy_in(legacy: &Path, dirs: &Dirs) -> Option<String> {
    if !needed(legacy, dirs) || failed_marker(dirs).exists() {
        return None;
    }
    match migrate(legacy, dirs, false) {
        Ok(outcome) if !outcome.moved.is_empty() => Some(format!(
            "pm: moved {} from {} into the XDG dirs; `pm doctor` names anything left",
            outcome.moved.join(", "),
            legacy.display()
        )),
        Ok(_) => None,
        Err(e) => {
            let _ = std::fs::create_dir_all(&dirs.state);
            let _ = std::fs::write(failed_marker(dirs), e.to_string());
            Some(format!(
                "pm: could not move its files from {} into the XDG dirs ({e}); \
                 everything stays there, and `pm upgrade` tries again",
                legacy.display()
            ))
        }
    }
}

/// `pm upgrade`'s run, as report lines, followed by the tmux config lines
/// that name the legacy dir.
pub fn upgrade_lines(dry_run: bool) -> Vec<String> {
    let Ok(home) = paths::home_dir() else {
        return Vec::new();
    };
    let dirs = paths::dirs_under(&home);
    let legacy = legacy_dir(&home, &dirs);
    let mut lines = if is_installed_pm() {
        upgrade_lines_in(&legacy, &dirs, dry_run)
    } else if needed(&legacy, &dirs) {
        vec![format!(
            "pm files: left in {}, as this pm is not the `pm` on PATH that every other \
             pm process runs; run `pm upgrade` with that one",
            legacy.display()
        )]
    } else {
        Vec::new()
    };
    lines.extend(tmux_conf_findings(&home, &legacy, &dirs));
    lines
}

/// [`upgrade_lines`]' run, but for the tmux config.
fn upgrade_lines_in(legacy: &Path, dirs: &Dirs, dry_run: bool) -> Vec<String> {
    let mut lines = Vec::new();
    match migrate(legacy, dirs, dry_run) {
        Ok(outcome) => {
            if !dry_run {
                let _ = std::fs::remove_file(failed_marker(dirs));
            }
            if !outcome.moved.is_empty() {
                let verb = if dry_run { "Would move" } else { "Moved" };
                lines.push(format!(
                    "{verb} {} from {} into the XDG dirs",
                    outcome.moved.join(", "),
                    legacy.display()
                ));
            }
            lines.extend(outcome_notes(&outcome, legacy, dirs));
        }
        Err(e) => lines.push(format!(
            "pm files: could not move them from {} ({e}); everything stays there",
            legacy.display()
        )),
    }
    lines
}

/// What a run left behind and why, as `pm upgrade` and `pm doctor` say it.
fn outcome_notes(outcome: &Outcome, legacy: &Path, dirs: &Dirs) -> Vec<String> {
    let mut notes = Vec::new();
    if outcome.conflict {
        notes.push(format!(
            "pm files — both {} and {} hold pm config, so only registry entries the first \
             alone had were moved; pm uses {}. Merge what you need into it and remove the other",
            legacy.display(),
            dirs.config.display(),
            dirs.config.display()
        ));
    }
    if !outcome.registered_in_both.is_empty() {
        notes.push(format!(
            "pm files — {} in {} are registered in {} too, so pm uses those; \
             compare and remove the legacy ones",
            outcome.registered_in_both.join(", "),
            legacy.display(),
            dirs.config.display()
        ));
    }
    if outcome.config_deferred {
        notes.push(format!(
            "pm files — a `pm serve` or tmux watcher of an earlier release still reads the \
             registry in {}; it moves on the first pm command after they restart (both \
             restart themselves when the pm binary is replaced)",
            legacy.display()
        ));
    }
    if outcome.serve_deferred {
        notes.push(format!(
            "pm files — a `pm serve` of an earlier release still runs from {}; its files \
             move once it restarts (`pm serve install`, or restart it under your service manager)",
            ServeFiles::legacy(legacy).dir.display()
        ));
    }
    notes
}
