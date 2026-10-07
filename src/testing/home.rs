//! The `$HOME` stand-in every global-tier path uses under `cfg(test)`.

use std::sync::OnceLock;

use super::pid_is_alive;

static TEST_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();

const TEST_HOME_PREFIX: &str = "pm-test-home-";

/// Remove `pm-test-home-<pid>` dirs left by test binaries that died without
/// running their atexit handler.
fn reap_dead_test_homes() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let self_pid = std::process::id();
    for entry in entries.flatten() {
        let Ok(fname) = entry.file_name().into_string() else {
            continue;
        };
        let Some(pid) = fname
            .strip_prefix(TEST_HOME_PREFIX)
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == self_pid || pid_is_alive(pid) {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

extern "C" fn atexit_remove_test_home() {
    if let Some(home) = TEST_HOME.get() {
        let _ = std::fs::remove_dir_all(home);
    }
}

/// The `$HOME` stand-in every global-tier path uses under `cfg(test)`: one
/// `pm-test-home-<pid>` temp dir per test binary, shared by all its tests.
/// The global asset tier is installed in it before any test sees it: a
/// first install writes temp files into directories a concurrent install is
/// listing and copying, while a later one finds everything up to date and
/// writes nothing. Tests may read the tier freely; a test that needs to
/// *mutate* it must use the explicit-dir variants against its own tempdir.
pub fn test_home() -> &'static std::path::Path {
    TEST_HOME.get_or_init(|| {
        reap_dead_test_homes();
        let dir = std::env::temp_dir().join(format!("{TEST_HOME_PREFIX}{}", std::process::id()));
        // Pid reuse: a stale dir under our own pid holds another run's state.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test home");
        // Safety: `libc::atexit` is always safe to call; the handler only
        // removes a directory.
        unsafe {
            libc::atexit(atexit_remove_test_home);
        }
        crate::commands::skills::install_global_in(&crate::commands::skills::GlobalStore::at(&dir))
            .expect("install the global tier into the test home");
        dir
    })
}
