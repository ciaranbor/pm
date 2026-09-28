//! Runs the opencode plugin's own tests (`plugins/opencode/pm-never-idle/
//! loop.test.ts`) under `node --test`, which needs a node that runs
//! TypeScript directly (22.18 or later).

use std::process::Command;

const TESTS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/plugins/opencode/pm-never-idle/loop.test.ts"
);

/// The installed node's `(major, minor)`.
fn node_version() -> Option<(u32, u32)> {
    let out = Command::new("node").arg("--version").output().ok()?;
    let version = String::from_utf8_lossy(&out.stdout);
    let mut parts = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??))
}

#[test]
fn never_idle_plugin_logic() {
    let version = node_version();
    assert!(
        version.is_some_and(|v| v >= (22, 18)),
        "the opencode plugin's tests need node >= 22.18 on PATH; found {version:?}"
    );
    let out = Command::new("node")
        .args(["--test", TESTS])
        .output()
        .expect("run node");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
