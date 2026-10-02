//! Harness binary probes (`--version`, `--help`), optionally answered from a
//! cache in the pm config dir so `pm status` doesn't spawn every harness in
//! use each time it runs. An entry is keyed by the resolved binary's path and
//! the probed argument, and is valid only while that file's size and mtime
//! are unchanged — an upgrade replaces or retargets the binary. A `#!`
//! wrapper's own file stays the same when what it runs is upgraded, so its
//! entry also holds the size and mtime of each file it names: by absolute
//! path, or by the name of a file beside it (`exec "$(dirname "$0")/x"`).
//! A target it computes any other way is not seen. Only a probe
//! that ran to an exit is stored; one that could not run is retried. The
//! cache is best-effort: an unreadable or unwritable file only costs a probe.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::fs_utils::write_atomic;
use crate::state::paths;

/// Whether a probe may be answered from the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// Reuse a stored result for the same unchanged binary.
    Cached,
    /// Run the binary, and store what it answered.
    Fresh,
}

/// How a probed binary exited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Exit {
    pub success: bool,
    pub stdout: String,
}

impl From<std::process::Output> for Exit {
    fn from(out: std::process::Output) -> Self {
        Exit {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    binary: PathBuf,
    stamp: String,
    exit: Exit,
}

/// `binary arg`'s exit, by `spawn` unless `probe` allows a cached one.
pub(super) fn run<E>(
    binary: &str,
    arg: &str,
    probe: Probe,
    spawn: impl FnOnce() -> Result<Exit, E>,
) -> Result<Exit, E> {
    match paths::global_config_dir() {
        Ok(dir) => run_in(&cache_file(&dir), binary, arg, probe, spawn),
        Err(_) => spawn(),
    }
}

/// The config dir's machine-local cache dir, which the probe cache is in.
pub(crate) const CACHE_DIR_NAME: &str = "cache";

pub(crate) fn cache_file(config_dir: &Path) -> PathBuf {
    config_dir.join(CACHE_DIR_NAME).join("harness-probes.json")
}

fn run_in<E>(
    cache: &Path,
    binary: &str,
    arg: &str,
    probe: Probe,
    spawn: impl FnOnce() -> Result<Exit, E>,
) -> Result<Exit, E> {
    let Some(path) = resolve(binary) else {
        return spawn();
    };
    let Some(stamp) = stamp(&path) else {
        return spawn();
    };
    let key = format!("{} {arg}", path.display());
    let mut entries = load(cache);
    if probe == Probe::Cached
        && let Some(entry) = entries.get(&key).filter(|e| e.stamp == stamp)
    {
        return Ok(entry.exit.clone());
    }
    let exit = spawn()?;
    entries.insert(
        key,
        Entry {
            binary: path,
            stamp,
            exit: exit.clone(),
        },
    );
    // A versioned install leaves its old path behind at each upgrade.
    entries.retain(|_, entry| entry.binary.exists());
    store(cache, &entries);
    Ok(exit)
}

/// The file `binary` runs, as found on `PATH`.
fn resolve(binary: &str) -> Option<PathBuf> {
    crate::fs_utils::resolve_binary(binary, std::env::var_os("PATH").as_deref())
}

/// A wrapper read for the files it names is at most this long.
const WRAPPER_MAX: u64 = 64 * 1024;

fn stamp(path: &Path) -> Option<String> {
    let mut stamp = file_stamp(path)?;
    for target in wrapped(path) {
        stamp.push(' ');
        stamp.push_str(&file_stamp(&target).unwrap_or_default());
    }
    Some(stamp)
}

fn file_stamp(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(format!("{}:{}", meta.len(), mtime.as_nanos()))
}

/// The files a `#!` script at `path` names: absolute paths, and names of
/// files in its own directory. Empty for anything else.
fn wrapped(path: &Path) -> Vec<PathBuf> {
    let small = std::fs::metadata(path).is_ok_and(|m| m.len() <= WRAPPER_MAX);
    let Some(text) = small.then(|| std::fs::read(path).ok()).flatten() else {
        return Vec::new();
    };
    if !text.starts_with(b"#!") {
        return Vec::new();
    }
    let dir = path.parent().unwrap_or(Path::new("/"));
    let text = String::from_utf8_lossy(&text);
    let mut files: Vec<PathBuf> = text
        .split(|c: char| !(c.is_alphanumeric() || "._-/+@".contains(c)))
        .flat_map(|token| {
            [
                Some(PathBuf::from(token)),
                token.rsplit('/').next().map(|n| dir.join(n)),
            ]
        })
        .flatten()
        .filter(|f| f.is_absolute() && f.as_path() != path && f.is_file())
        .collect();
    files.sort();
    files.dedup();
    files
}

fn load(cache: &Path) -> BTreeMap<String, Entry> {
    std::fs::read_to_string(cache)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn store(cache: &Path, entries: &BTreeMap<String, Entry>) {
    if let Ok(json) = serde_json::to_string_pretty(entries) {
        let _ = write_atomic(cache, json.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn cached_probe_reuses_an_unchanged_binarys_answer_and_fresh_reprobes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache/harness-probes.json");
        let binary = dir.path().join("harness");
        std::fs::write(&binary, "v1").unwrap();
        let binary = binary.to_str().unwrap();
        let spawns = Cell::new(0);
        let probe = |probe, stdout: &str| {
            run_in(&cache, binary, "--version", probe, || {
                spawns.set(spawns.get() + 1);
                Ok::<_, ()>(Exit {
                    success: true,
                    stdout: stdout.to_string(),
                })
            })
            .unwrap()
            .stdout
        };

        assert_eq!(probe(Probe::Cached, "1.0"), "1.0");
        assert_eq!(probe(Probe::Cached, "ignored"), "1.0");
        assert_eq!(spawns.get(), 1);

        assert_eq!(probe(Probe::Fresh, "1.1"), "1.1");
        assert_eq!(probe(Probe::Cached, "ignored"), "1.1");
        assert_eq!(spawns.get(), 2);

        std::fs::write(binary, "v2 upgraded").unwrap();
        assert_eq!(probe(Probe::Cached, "2.0"), "2.0");
        assert_eq!(spawns.get(), 3);

        // An upgrade to a new versioned path drops the old path's entry.
        let upgraded = dir.path().join("harness-3");
        std::fs::write(&upgraded, "v3").unwrap();
        std::fs::remove_file(binary).unwrap();
        run_in(
            &cache,
            upgraded.to_str().unwrap(),
            "--version",
            Probe::Cached,
            || {
                Ok::<_, ()>(Exit {
                    success: true,
                    stdout: "3.0".to_string(),
                })
            },
        )
        .unwrap();
        assert_eq!(
            load(&cache)
                .into_values()
                .map(|e| e.binary)
                .collect::<Vec<_>>(),
            vec![upgraded.canonicalize().unwrap()]
        );
    }

    #[test]
    fn a_wrapper_is_probed_again_when_the_file_beside_it_that_it_runs_changes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("harness-probes.json");
        let wrapper = dir.path().join("harness");
        std::fs::write(
            &wrapper,
            "#!/bin/sh\nexec \"$(dirname \"$0\")/real\" \"$@\"\n",
        )
        .unwrap();
        let real = dir.path().join("real");
        std::fs::write(&real, "v1").unwrap();
        let spawns = Cell::new(0);
        let probe = || {
            run_in(
                &cache,
                wrapper.to_str().unwrap(),
                "--version",
                Probe::Cached,
                || {
                    spawns.set(spawns.get() + 1);
                    Ok::<_, ()>(Exit {
                        success: true,
                        stdout: String::new(),
                    })
                },
            )
            .unwrap();
            spawns.get()
        };

        assert_eq!(probe(), 1);
        assert_eq!(probe(), 1, "cached");
        std::fs::write(&real, "v2, upgraded").unwrap();
        assert_eq!(probe(), 2, "its target changed");
        assert_eq!(probe(), 2);
    }

    #[test]
    fn a_probe_that_could_not_run_is_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("harness-probes.json");
        let binary = dir.path().join("harness");
        std::fs::write(&binary, "").unwrap();
        let binary = binary.to_str().unwrap();

        assert!(run_in(&cache, binary, "--help", Probe::Cached, || Err(())).is_err());
        let exit = run_in(&cache, binary, "--help", Probe::Cached, || {
            Ok::<_, ()>(Exit {
                success: true,
                stdout: "usage".to_string(),
            })
        });
        assert_eq!(exit.unwrap().stdout, "usage");
    }
}
