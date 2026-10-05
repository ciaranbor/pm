//! `scripts/release`'s clean-tree check, run as a dry run against a temp
//! checkout of a throwaway project pushed to a local bare origin.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/release");

fn run(program: &str, dir: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("spawn")
}

fn git(dir: &Path, args: &[&str]) {
    let out = run("git", dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A checkout of `main` in sync with its origin, releasable as 0.2.0.
fn checkout() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let origin = tmp.path().join("origin.git");
    let work = tmp.path().join("work");
    fs::create_dir_all(work.join("scripts")).unwrap();
    git(
        tmp.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(&work, &["init", "-q", "-b", "main"]);
    fs::copy(SCRIPT, work.join("scripts/release")).unwrap();
    fs::write(
        work.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        work.join("CHANGELOG.md"),
        "# Changelog\n\n## Unreleased\n\n- a\n",
    )
    .unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "init"]);
    git(
        &work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&work, &["push", "-q", "-u", "origin", "main"]);
    (tmp, work)
}

fn dry_run(work: &Path) -> Output {
    run("bash", work, &["scripts/release", "0.2.0", "--dry-run"])
}

#[test]
fn untracked_files_do_not_block_a_release() {
    let (_tmp, work) = checkout();
    fs::create_dir(work.join("notes")).unwrap();
    fs::write(work.join("notes/a.md"), "a").unwrap();
    fs::write(work.join("notes/b.md"), "b").unwrap();
    let out = dry_run(&work);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("untracked files are left out of the release: notes/\n"));
    assert!(stdout.contains("(dry run: nothing changed)"));
}

#[test]
fn tracked_changes_block_a_release() {
    let (_tmp, work) = checkout();
    fs::write(work.join("CHANGELOG.md"), "edited").unwrap();
    let out = dry_run(&work);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("uncommitted changes"));
}

#[test]
fn a_staged_file_blocks_a_release() {
    let (_tmp, work) = checkout();
    fs::write(work.join("new.txt"), "x").unwrap();
    git(&work, &["add", "new.txt"]);
    let out = dry_run(&work);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("uncommitted changes"));
}
