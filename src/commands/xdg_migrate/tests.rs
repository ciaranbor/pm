use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::*;

/// The XDG dirs under `root/home`, the state dir moved to `state_home`
/// (and the runtime dir kept out of it).
fn dirs_in(root: &Path, state_home: Option<&Path>) -> Dirs {
    let state: Option<OsString> = state_home.map(|p| p.into());
    let runtime: OsString = root.join("run").into();
    Dirs::resolve(&root.join("home"), |name| match name {
        "XDG_STATE_HOME" => state.clone(),
        "XDG_RUNTIME_DIR" => state.as_ref().map(|_| runtime.clone()),
        _ => None,
    })
}

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

const CONFIG_FILES: &[&str] = &[
    "config.toml",
    "notices.md",
    "projects/p.toml",
    "workflows/w/config.toml",
    ".git/HEAD",
    ".gitignore",
];

/// An earlier release's dir at `legacy`: every file holds its own path.
fn seed(legacy: &Path, with_config: bool) {
    if with_config {
        for f in CONFIG_FILES {
            write(&legacy.join(f), f);
        }
    }
    for f in [
        "cache/harness-probes.json",
        "serve/devices.toml",
        "serve/vapid.pem",
        "serve/serve.log",
        "serve/state.json",
        "serve/serve.lock",
        "tmux/sock.watch.lock",
        "registry-before-pull/p.toml",
    ] {
        write(&legacy.join(f), f);
    }
    let made = std::process::Command::new("mkfifo")
        .arg(legacy.join("serve/wake"))
        .status()
        .unwrap();
    assert!(made.success());
    for secret in ["serve/devices.toml", "serve/vapid.pem"] {
        std::fs::set_permissions(legacy.join(secret), std::fs::Permissions::from_mode(0o600))
            .unwrap();
    }
}

fn assert_machine_files_moved(legacy_rel: impl Fn(&str) -> String, dirs: &Dirs) {
    let serve = ServeFiles::in_dirs(dirs);
    for (at, was) in [
        (
            dirs.cache.join("harness-probes.json"),
            "cache/harness-probes.json",
        ),
        (serve.devices(), "serve/devices.toml"),
        (serve.key(), "serve/vapid.pem"),
        (serve.log(), "serve/serve.log"),
        (serve.state(), "serve/state.json"),
        (
            dirs.state.join("registry-before-pull/p.toml"),
            "registry-before-pull/p.toml",
        ),
    ] {
        assert_eq!(
            std::fs::read_to_string(&at).unwrap(),
            legacy_rel(was),
            "{}",
            at.display()
        );
    }
    assert_eq!(mode(&serve.dir), 0o700);
    assert_eq!(mode(&serve.devices()), 0o600);
    assert_eq!(mode(&serve.key()), 0o600);
}

/// [`migrate`] until it leaves nothing at `legacy`.
fn migrate_once_free(legacy: &Path, dirs: &Dirs) -> Outcome {
    migrate_until(legacy, dirs, |_| !legacy.exists())
}

/// [`migrate`] until `done`, for up to 10s: a child another test forks holds
/// a copy of every open fd until it execs, so a lock just released, or one
/// `held` takes to probe it, can read as held for a moment.
fn migrate_until(legacy: &Path, dirs: &Dirs, done: impl Fn(&Outcome) -> bool) -> Outcome {
    let start = std::time::Instant::now();
    loop {
        let outcome = migrate(legacy, dirs, false).unwrap();
        if done(&outcome) {
            return outcome;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "{outcome:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn everything_moves_once_and_the_legacy_dir_goes() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("Application Support/pm");
    let dirs = dirs_in(root.path(), None);
    seed(&legacy, true);

    let outcome = migrate_once_free(&legacy, &dirs);
    assert!(!outcome.conflict);
    for f in CONFIG_FILES {
        assert_eq!(std::fs::read_to_string(dirs.config.join(f)).unwrap(), *f);
    }
    assert_machine_files_moved(|f| f.to_string(), &dirs);
    assert!(
        !legacy.exists(),
        "locks, FIFO and cache deleted, dir removed"
    );

    assert!(!needed(&legacy, &dirs));
    assert_eq!(migrate(&legacy, &dirs, false).unwrap(), Outcome::default());
}

#[test]
fn a_dry_run_names_the_moves_and_makes_none() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    let dirs = dirs_in(root.path(), None);
    seed(&legacy, true);

    let outcome = migrate(&legacy, &dirs, true).unwrap();
    assert!(
        outcome.moved.contains(&"projects".to_string()),
        "{outcome:?}"
    );
    assert!(outcome.moved.contains(&"serve/vapid.pem".to_string()));
    assert!(legacy.join("projects/p.toml").exists());
    assert!(!dirs.config.exists());
}

#[test]
fn where_the_config_dir_was_already_xdg_only_machine_files_move_out_of_it() {
    let root = tempfile::tempdir().unwrap();
    let dirs = dirs_in(root.path(), None);
    let legacy = dirs.config.clone();
    seed(&legacy, true);

    migrate(&legacy, &dirs, false).unwrap();
    assert_machine_files_moved(|f| f.to_string(), &dirs);
    for f in CONFIG_FILES {
        assert!(dirs.config.join(f).exists(), "{f}");
    }
    for d in LEGACY_MACHINE_DIRS {
        assert!(!dirs.config.join(d).exists(), "{d}");
    }
    assert!(!needed(&legacy, &dirs));
}

#[test]
fn config_in_both_dirs_stays_but_entries_only_the_legacy_dir_registers_move() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    let dirs = dirs_in(root.path(), None);
    seed(&legacy, true);
    write(&legacy.join("projects/both.toml"), "legacy");
    write(&dirs.config.join("config.toml"), "new");
    write(&dirs.config.join("projects/both.toml"), "new");

    let outcome = migrate_until(&legacy, &dirs, |o| !o.config_deferred && !o.serve_deferred);
    assert!(outcome.conflict);
    assert_eq!(outcome.registered_in_both, ["projects/both.toml"]);
    let read = |p: &Path| std::fs::read_to_string(p).unwrap();
    assert_eq!(read(&dirs.config.join("config.toml")), "new");
    assert_eq!(read(&dirs.config.join("projects/both.toml")), "new");
    assert_eq!(
        read(&dirs.config.join("projects/p.toml")),
        "projects/p.toml"
    );
    assert!(!legacy.join("projects/p.toml").exists());
    assert_eq!(read(&legacy.join("projects/both.toml")), "legacy");
    assert!(legacy.join("config.toml").exists());
    assert_machine_files_moved(|f| f.to_string(), &dirs);

    let warnings = doctor::warnings_in(&legacy, &dirs).join("\n");
    assert!(warnings.contains("both"), "{warnings}");
    assert!(warnings.contains("projects/both.toml"), "{warnings}");
    assert!(warnings.contains("config.toml"), "{warnings}");
}

#[test]
fn only_the_pm_on_path_counts_as_installed() {
    let root = tempfile::tempdir().unwrap();
    let installed = root.path().join("cellar/pm");
    write(&installed, "");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&installed, bin.join("pm")).unwrap();
    let dev = root.path().join("target/debug/pm");
    write(&dev, "");
    for f in [&installed, &dev] {
        std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::join_paths([bin, dev.parent().unwrap().to_path_buf()]).unwrap();

    assert!(is_on_path(&installed, Some(&path)));
    assert!(!is_on_path(&dev, Some(&path)));
    assert!(!is_on_path(&dev, None));
}

#[test]
fn a_failure_midway_puts_everything_back() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    let locked = crate::testing::lock_in(root.path());
    let dirs = dirs_in(root.path(), Some(&locked.join("state")));
    seed(&legacy, true);

    let result = migrate(&legacy, &dirs, false);
    crate::testing::unlock(&locked);
    assert!(result.is_err());
    for f in CONFIG_FILES {
        assert_eq!(std::fs::read_to_string(legacy.join(f)).unwrap(), *f);
        assert!(!dirs.config.join(f).exists(), "{f}");
    }
    assert!(legacy.join("cache/harness-probes.json").exists());
    assert!(!dirs.cache.join("harness-probes.json").exists());
    assert!(
        legacy.join("serve/serve.lock").exists(),
        "nothing cleaned up"
    );
}

#[test]
fn a_running_old_server_keeps_its_files_and_the_registry_it_reads() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    let dirs = dirs_in(root.path(), None);
    seed(&legacy, true);
    let old_server = std::fs::File::open(legacy.join("serve/serve.lock")).unwrap();
    old_server.lock().unwrap();

    let outcome = migrate(&legacy, &dirs, false).unwrap();
    assert!(outcome.serve_deferred);
    for f in [
        "devices.toml",
        "vapid.pem",
        "serve.log",
        "serve.lock",
        "wake",
    ] {
        assert!(legacy.join("serve").join(f).exists(), "{f}");
    }
    assert!(outcome.config_deferred);
    assert!(legacy.join("projects/p.toml").exists());
    assert!(dirs.cache.join("harness-probes.json").exists());
    assert!(!ServeFiles::in_dirs(&dirs).dir.exists());

    drop(old_server);
    let outcome = migrate_once_free(&legacy, &dirs);
    assert!(!outcome.serve_deferred && !outcome.config_deferred);
    assert_machine_files_moved(|f| f.to_string(), &dirs);
    assert!(dirs.config.join("projects/p.toml").exists());
    assert!(!legacy.exists());
}

#[test]
fn a_log_the_new_server_already_writes_is_kept_and_the_old_one_set_beside_it() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    let dirs = dirs_in(root.path(), None);
    seed(&legacy, false);
    let serve = ServeFiles::in_dirs(&dirs);
    write(&serve.log(), "new");

    migrate(&legacy, &dirs, false).unwrap();
    assert_eq!(std::fs::read_to_string(serve.log()).unwrap(), "new");
    assert_eq!(
        std::fs::read_to_string(serve.dir.join("serve.log.old")).unwrap(),
        "serve/serve.log"
    );
}

#[test]
fn a_copy_across_filesystems_lands_whole() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    write(&src.join("a/b.toml"), "b");
    std::os::unix::fs::symlink("a/b.toml", src.join("link")).unwrap();
    let dst = root.path().join("dst/projects");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();

    moves::copy_into(&src, &dst).unwrap();
    assert_eq!(std::fs::read_to_string(dst.join("a/b.toml")).unwrap(), "b");
    assert_eq!(
        std::fs::read_link(dst.join("link")).unwrap(),
        Path::new("a/b.toml")
    );
    assert_eq!(
        std::fs::read_dir(dst.parent().unwrap()).unwrap().count(),
        1,
        "no temp copy left"
    );
}

#[test]
fn doctor_warns_of_serve_files_others_can_read() {
    let root = tempfile::tempdir().unwrap();
    let files = ServeFiles::in_dirs(&dirs_in(root.path(), None));
    write(&files.key(), "k");
    std::fs::set_permissions(&files.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(files.key(), std::fs::Permissions::from_mode(0o644)).unwrap();

    let warnings = doctor::permission_warnings(&files);
    assert_eq!(warnings.len(), 2, "{warnings:?}");

    files.create().unwrap();
    std::fs::set_permissions(files.key(), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(doctor::permission_warnings(&files).is_empty());
}

#[test]
fn a_tmux_config_naming_the_legacy_dir_is_reported_and_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let dirs = dirs_in(root.path(), None);
    let legacy = home.join("Library/Application Support/pm");
    let conf = "run-shell 'pm tmux init'\nsource ~/Library/Application Support/pm/x.conf\n";
    write(&home.join(".tmux.conf"), conf);
    write(
        &home.join(".config/tmux/tmux.conf"),
        "run-shell 'pm tmux init'\n",
    );

    let findings = tmux_conf_findings(&home, &legacy, &dirs);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains(".tmux.conf"), "{findings:?}");
    assert_eq!(
        std::fs::read_to_string(home.join(".tmux.conf")).unwrap(),
        conf
    );
}

#[test]
fn an_old_tmux_watcher_keeps_the_config_in_place_until_it_lets_go() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    let dirs = dirs_in(root.path(), None);
    seed(&legacy, true);
    let watcher = std::fs::File::open(legacy.join("tmux/sock.watch.lock")).unwrap();
    watcher.lock().unwrap();

    let outcome = migrate(&legacy, &dirs, false).unwrap();
    assert!(outcome.config_deferred);
    assert!(legacy.join("projects/p.toml").exists());
    assert!(legacy.join("tmux/sock.watch.lock").exists());
    assert_machine_files_moved(|f| f.to_string(), &dirs);

    drop(watcher);
    migrate_once_free(&legacy, &dirs);
    assert!(dirs.config.join("projects/p.toml").exists());
    assert!(!legacy.exists());
}

#[test]
fn a_run_with_nothing_it_can_do_takes_no_lock() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    let dirs = dirs_in(root.path(), None);
    seed(&legacy, true);
    write(&dirs.config.join("config.toml"), "new");
    migrate(&legacy, &dirs, false).unwrap();

    let locked = crate::testing::lock_in(root.path());
    let unlockable = Dirs {
        runtime: locked.join("run"),
        ..dirs
    };
    let outcome = migrate(&legacy, &unlockable, false);
    crate::testing::unlock(&locked);
    assert!(outcome.unwrap().conflict);
}

#[test]
fn an_earlier_old_log_is_kept_too() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    let dirs = dirs_in(root.path(), None);
    seed(&legacy, false);
    let serve = ServeFiles::in_dirs(&dirs);
    write(&serve.log(), "new");
    write(&serve.dir.join("serve.log.old"), "older");

    migrate_once_free(&legacy, &dirs);
    assert_eq!(
        std::fs::read_to_string(serve.dir.join("serve.log.old.2")).unwrap(),
        "serve/serve.log"
    );
    assert!(!legacy.exists());
}

#[test]
fn after_a_failed_run_only_upgrade_tries_again_and_clears_the_marker() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    let dirs = dirs_in(root.path(), None);
    seed(&legacy, true);
    write(&failed_marker(&dirs), "earlier failure");

    assert_eq!(lazy_in(&legacy, &dirs), None);
    assert!(legacy.join("projects").exists());
    let warnings = doctor::warnings_in(&legacy, &dirs).join("\n");
    assert!(warnings.contains("earlier failure"), "{warnings}");

    let lines = upgrade_lines_in(&legacy, &dirs, false);
    assert!(lines[0].starts_with("Moved "), "{lines:?}");
    assert!(!failed_marker(&dirs).exists());
    assert!(dirs.config.join("projects/p.toml").exists());
}
