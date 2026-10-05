//! Sets `PM_VERSION`, the version pm reports, and `PM_TARGET`, the target
//! triple a self-update fetches the binary of.
//!
//! A release build (`PM_RELEASE=1`, as CI builds) reports the Cargo.toml
//! version alone. Any other build appends what `git describe` says of the
//! checkout as semver build metadata — `0.2.0+3.gabc1234.dirty` — so a
//! build from source is never mistaken for the release it started from.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=PM_RELEASE");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!(
        "cargo:rustc-env=PM_TARGET={}",
        std::env::var("TARGET").expect("cargo sets TARGET")
    );
    let base = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let version = if std::env::var("PM_RELEASE").as_deref() == Ok("1") {
        base
    } else {
        watch_git(&manifest);
        match describe(&manifest) {
            Some(build) => format!("{base}+{build}"),
            None => base,
        }
    };
    println!("cargo:rustc-env=PM_VERSION={version}");
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The build metadata for the checkout at `dir`: commits since the last
/// `v*` tag, the commit, and whether the tree is dirty; `None` outside git.
fn describe(dir: &Path) -> Option<String> {
    let described = git(
        dir,
        &["describe", "--tags", "--long", "--dirty", "--match", "v*"],
    );
    let (described, tagged) = match described {
        Some(d) => (d, true),
        None => (git(dir, &["describe", "--always", "--dirty"])?, false),
    };
    let (described, dirty) = match described.strip_suffix("-dirty") {
        Some(clean) => (clean.to_string(), true),
        None => (described, false),
    };
    let mut parts = Vec::new();
    if tagged {
        // `<tag>-<n>-g<sha>`; the tag itself may contain dashes.
        let mut fields = described.rsplitn(3, '-');
        let sha = fields.next()?;
        let count = fields.next()?;
        parts.push(count.to_string());
        parts.push(sha.to_string());
    } else {
        parts.push(format!("g{described}"));
    }
    if dirty {
        parts.push("dirty".into());
    }
    Some(parts.join("."))
}

/// Rerun when the checkout's commit or tags move. `HEAD` lives in the
/// worktree's git dir; refs live in the common dir all worktrees share.
fn watch_git(dir: &Path) {
    let (Some(git_dir), Some(common)) = (
        git(dir, &["rev-parse", "--absolute-git-dir"]),
        git(
            dir,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ),
    ) else {
        return;
    };
    let (git_dir, common) = (PathBuf::from(git_dir), PathBuf::from(common));
    let mut watched = vec![
        git_dir.join("HEAD"),
        common.join("packed-refs"),
        common.join("refs/tags"),
    ];
    if let Some(branch) = git(dir, &["symbolic-ref", "-q", "HEAD"]) {
        watched.push(common.join(branch));
    }
    for path in watched.iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
