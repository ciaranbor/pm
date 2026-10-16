//! Which opencode is installed, and whether pm supports it.

use super::api::executable;
use super::binary;
use crate::bounded;
use crate::harness::probe::{self, Probe, parse_version};
use crate::state::project::OpenCodeConfig;

/// Earliest release verified against the events and plugin API the loop
/// relies on and the prompt box [`input`](super::input) reads; an older box reads as
/// unknown, which refuses remote input.
const MIN_VERSION: (u32, u32, u32) = (2, 0, 23);

/// The installed version's raw string, or why opencode could not be asked.
/// `--version` starts no server, so it needs no `--standalone`.
pub(in crate::harness) fn installed_version(
    cfg: &OpenCodeConfig,
    probe: Probe,
) -> std::result::Result<String, String> {
    let unrunnable = || format!("`{}` could not be run", binary(cfg));
    let exit = probe::run(binary(cfg), executable(binary(cfg)), "--version", probe).map_err(
        |failure| match failure {
            bounded::Failure::TimedOut { .. } => failure.describe("opencode --version"),
            bounded::Failure::Unrunnable { .. } => unrunnable(),
        },
    )?;
    if !exit.success {
        return Err(unrunnable());
    }
    Ok(exit.stdout.trim().to_string())
}

/// Whether the installed opencode is at least [`MIN_VERSION`]; `None` when
/// it can't be probed.
pub(in crate::harness) fn version_supported(cfg: &OpenCodeConfig, probe: Probe) -> Option<bool> {
    Some(parse_version(&installed_version(cfg, probe).ok()?)? >= MIN_VERSION)
}

pub(in crate::harness) fn unusable_reason(cfg: &OpenCodeConfig, probe: Probe) -> Option<String> {
    let found = match installed_version(cfg, probe) {
        Ok(found) => found,
        Err(reason) => {
            return Some(format!(
                "{reason}; install opencode or set `[harness.opencode] binary`"
            ));
        }
    };
    match parse_version(&found) {
        Some(version) if version >= MIN_VERSION => None,
        _ => Some(format!(
            "installed opencode is `{found}`; pm needs {} or later",
            min_version_string()
        )),
    }
}

pub(in crate::harness) fn min_version_string() -> String {
    let (a, b, c) = MIN_VERSION;
    format!("{a}.{b}.{c}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::opencode::test_support::fake_opencode;

    #[test]
    fn unusable_reason_names_a_missing_or_old_binary() {
        let dir = tempfile::tempdir().unwrap();
        let missing = OpenCodeConfig {
            binary: Some(dir.path().join("nope").to_string_lossy().into_owned()),
            ..Default::default()
        };
        let reason = unusable_reason(&missing, Probe::Fresh).unwrap();
        assert!(reason.contains("could not be run"), "{reason}");
        assert_eq!(version_supported(&missing, Probe::Fresh), None);

        let old = fake_opencode(dir.path(), "opencode v2.0.22", 0);
        assert_eq!(
            unusable_reason(&old, Probe::Fresh).unwrap(),
            "installed opencode is `opencode v2.0.22`; pm needs 2.0.23 or later"
        );
        assert_eq!(version_supported(&old, Probe::Fresh), Some(false));

        let current = fake_opencode(dir.path(), "opencode v2.0.23", 0);
        assert_eq!(unusable_reason(&current, Probe::Fresh), None);
        assert_eq!(version_supported(&current, Probe::Fresh), Some(true));
    }
}
