//! The binary run as a user would: one module per area, sharing the
//! sandboxed `pm` invocations defined here.

mod cross_project;
mod help;
mod outside_project;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

/// The binary under test in a throwaway home of its own, which lives as
/// long as the command.
struct Pm {
    cmd: Command,
    _home: tempfile::TempDir,
}

impl std::ops::Deref for Pm {
    type Target = Command;
    fn deref(&self) -> &Command {
        &self.cmd
    }
}

impl std::ops::DerefMut for Pm {
    fn deref_mut(&mut self) -> &mut Command {
        &mut self.cmd
    }
}

fn pm() -> Pm {
    let home = tempdir().unwrap();
    Pm {
        cmd: Command::from_std(std_pm(home.path())),
        _home: home,
    }
}

/// The binary under test, reaching none of the user's own pm state: `home`
/// for `HOME`, no XDG dirs, no tmux server anyone uses, and no `pm` on
/// `PATH`, so it is never the installed pm.
fn std_pm(home: &std::path::Path) -> std::process::Command {
    let mut pm = std::process::Command::new(assert_cmd::cargo::cargo_bin("pm"));
    pm.envs(no_tmux())
        .env("HOME", home)
        .env("PATH", path_without_pm())
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("PM_AGENT_NAME")
        .env_remove(pm::state::paths::AGENT_WORKTREE_ENV);
    for var in pm::state::dirs::XDG_VARS {
        pm.env_remove(var);
    }
    pm
}

fn path_without_pm() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::split_paths(&path).filter(|dir| !dir.join("pm").exists());
    std::env::join_paths(dirs).unwrap()
}

/// A tmux server no test starts, so the binary under test never writes to
/// the default server or the one the run is inside.
fn no_tmux() -> [(&'static str, String); 1] {
    [(
        "PM_TMUX_SERVER",
        format!("pm-test-{}-cli-none", std::process::id()),
    )]
}
