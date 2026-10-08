//! Where pm keeps its global files, by the XDG Base Directory spec on every
//! OS, macOS included: config (`$XDG_CONFIG_HOME/pm`, the git-backed
//! registry repo), state (`$XDG_STATE_HOME/pm`, machine-written files kept
//! across runs, `pm serve`'s secrets among them), cache (`$XDG_CACHE_HOME/pm`,
//! disposable) and runtime (`$XDG_RUNTIME_DIR/pm`, locks and the wake FIFO).
//! A variable counts only when set to an absolute path; the spec says to
//! ignore a relative one. With no `$XDG_RUNTIME_DIR`, runtime files go
//! under the state dir rather than a shared `/tmp`.
//!
//! An earlier release kept everything under `dirs::config_dir()/pm`, which
//! on macOS is `~/Library/Application Support/pm` ([`legacy_dir`]);
//! [`xdg_migrate`](crate::commands::xdg_migrate) moves it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const NAME: &str = "pm";

/// What the config dir holds, all of it synced by its git repo.
pub const CONFIG_ITEMS: &[&str] = &[
    "config.toml",
    "notices.md",
    "workflows",
    "projects",
    ".git",
    ".gitignore",
];

/// The variables [`Dirs::resolve`] reads.
pub const XDG_VARS: &[&str] = &[
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_DATA_HOME",
];

/// pm's resolved global dirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub runtime: PathBuf,
    /// `$XDG_DATA_HOME` itself, not pm's dir under it: other tools' data
    /// (opencode's database) is found there.
    pub data_home: PathBuf,
}

impl Dirs {
    /// The dirs under `home`, reading the XDG variables through `env`.
    pub fn resolve(home: &Path, env: impl Fn(&str) -> Option<OsString>) -> Self {
        let var = |name: &str| env(name).map(PathBuf::from).filter(|dir| dir.is_absolute());
        let base = |name: &str, default: &str| var(name).unwrap_or_else(|| home.join(default));
        let state = base("XDG_STATE_HOME", ".local/state").join(NAME);
        Self {
            config: base("XDG_CONFIG_HOME", ".config").join(NAME),
            cache: base("XDG_CACHE_HOME", ".cache").join(NAME),
            runtime: var("XDG_RUNTIME_DIR").map_or_else(|| state.join("run"), |d| d.join(NAME)),
            data_home: base("XDG_DATA_HOME", ".local/share"),
            state,
        }
    }
}

/// Where an earlier release kept every global file: `~/Library/Application
/// Support/pm` on macOS; elsewhere the config dir itself, whose machine-local
/// subdirs then move out of it.
pub fn legacy_dir(home: &Path, dirs: &Dirs) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support").join(NAME)
    } else {
        dirs.config.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(vars: &[(&str, &str)]) -> Dirs {
        Dirs::resolve(Path::new("/home/u"), |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        })
    }

    #[test]
    fn defaults_hang_off_home_with_runtime_under_state() {
        assert_eq!(
            resolve(&[]),
            Dirs {
                config: "/home/u/.config/pm".into(),
                state: "/home/u/.local/state/pm".into(),
                cache: "/home/u/.cache/pm".into(),
                runtime: "/home/u/.local/state/pm/run".into(),
                data_home: "/home/u/.local/share".into(),
            }
        );
    }

    #[test]
    fn each_variable_moves_its_dir() {
        let dirs = resolve(&[
            ("XDG_CONFIG_HOME", "/c"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_CACHE_HOME", "/k"),
            ("XDG_RUNTIME_DIR", "/run/user/1"),
            ("XDG_DATA_HOME", "/d"),
        ]);
        assert_eq!(dirs.config, Path::new("/c/pm"));
        assert_eq!(dirs.state, Path::new("/s/pm"));
        assert_eq!(dirs.cache, Path::new("/k/pm"));
        assert_eq!(dirs.runtime, Path::new("/run/user/1/pm"));
        assert_eq!(dirs.data_home, Path::new("/d"));
    }

    #[test]
    fn empty_and_relative_values_are_ignored() {
        let dirs = resolve(&[
            ("XDG_CONFIG_HOME", ""),
            ("XDG_STATE_HOME", "rel/state"),
            ("XDG_RUNTIME_DIR", "run"),
        ]);
        assert_eq!(dirs, resolve(&[]));
    }

    #[test]
    fn runtime_falls_back_under_a_custom_state_dir() {
        let dirs = resolve(&[("XDG_STATE_HOME", "/s")]);
        assert_eq!(dirs.runtime, Path::new("/s/pm/run"));
    }
}
