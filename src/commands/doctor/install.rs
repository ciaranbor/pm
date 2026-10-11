//! Warnings about the installation itself: more than one `pm` on `PATH`,
//! where an update can land in one while the shell keeps running the
//! other, and a tmux older than the release pm requires.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::Depth;
use crate::tmux;

/// The oldest tmux pm supports (README's requirements).
const MIN_TMUX: (u32, u32) = (3, 6);

/// Asked only at full depth, as [`super::warnings::keychain_warning`] is.
pub(super) fn install_warnings(depth: Depth) -> Vec<String> {
    if depth != Depth::Full {
        return Vec::new();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let tmux = tmux::version().ok();
    pm_on_path_warning(&pms_on_path(&path))
        .into_iter()
        .chain(tmux.as_deref().and_then(tmux_version_warning))
        .collect()
}

/// Every executable `pm` in `path`'s directories, in order, one per
/// binary: a symlink to one already found is the same `pm`.
fn pms_on_path(path: &OsStr) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    std::env::split_paths(path)
        .map(|dir| dir.join("pm"))
        .filter(|pm| {
            std::fs::metadata(pm)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .filter(|pm| seen.insert(std::fs::canonicalize(pm).unwrap_or_else(|_| pm.clone())))
        .collect()
}

fn pm_on_path_warning(pms: &[PathBuf]) -> Option<String> {
    if pms.len() < 2 {
        return None;
    }
    let all: Vec<String> = pms.iter().map(|pm| pm.display().to_string()).collect();
    Some(format!(
        "install — {} pm binaries are on PATH ({}); `pm` runs the first, so one installed \
         or updated elsewhere is not the one that runs: remove all but one",
        pms.len(),
        all.join(", ")
    ))
}

/// `tmux -V`'s output, warned about when the release it names is older
/// than [`MIN_TMUX`]. A build it can't read a release from (`tmux master`)
/// is not warned about.
fn tmux_version_warning(version: &str) -> Option<String> {
    let found = release(version)?;
    (found < MIN_TMUX).then(|| {
        let (major, minor) = MIN_TMUX;
        format!(
            "tmux — {} is older than {major}.{minor}, the oldest pm supports: the plugin's \
             status, alerts and attention key break; upgrade tmux",
            version.trim()
        )
    })
}

/// The `(major, minor)` release in `tmux -V` output: `tmux 3.5a`,
/// `tmux next-3.6`, `tmux 3.6`.
fn release(version: &str) -> Option<(u32, u32)> {
    let number = version.trim().strip_prefix("tmux ")?;
    let number = number.strip_prefix("next-").unwrap_or(number);
    let (major, rest) = number.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_tmux_older_than_the_minimum_is_warned_about() {
        let old = tmux_version_warning("tmux 3.5a\n").expect("3.5a is too old");
        assert!(old.contains("tmux 3.5a is older than 3.6"), "{old}");
        assert!(tmux_version_warning("tmux 3.4").is_some());
        assert!(tmux_version_warning("tmux 2.9").is_some());
        for fine in [
            "tmux 3.6",
            "tmux 3.6a",
            "tmux next-3.7",
            "tmux 4.0",
            "tmux master",
        ] {
            assert_eq!(tmux_version_warning(fine), None, "{fine}");
        }
    }

    #[test]
    fn more_than_one_pm_on_path_is_warned_about_but_a_symlink_to_one_is_not_another() {
        let dir = tempdir().unwrap();
        let bin = |name: &str| {
            let dir = dir.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            dir
        };
        let (cargo, local, linked, plain) =
            (bin("cargo"), bin("local"), bin("linked"), bin("plain"));
        for at in [&cargo, &local] {
            std::fs::write(at.join("pm"), "").unwrap();
            std::fs::set_permissions(at.join("pm"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        std::os::unix::fs::symlink(cargo.join("pm"), linked.join("pm")).unwrap();
        std::fs::write(plain.join("pm"), "").unwrap();
        let path = |dirs: &[&PathBuf]| std::env::join_paths(dirs).unwrap();

        assert_eq!(
            pms_on_path(&path(&[&cargo, &linked, &plain])),
            [cargo.join("pm")]
        );
        assert_eq!(
            pm_on_path_warning(&pms_on_path(&path(&[&cargo, &linked]))),
            None
        );

        let both = pms_on_path(&path(&[&local, &linked, &cargo]));
        assert_eq!(both, [local.join("pm"), linked.join("pm")]);
        let warning = pm_on_path_warning(&both).expect("two pm binaries");
        assert!(warning.contains("2 pm binaries"), "{warning}");
        assert!(
            warning.contains(&local.join("pm").display().to_string()),
            "{warning}"
        );
    }
}
