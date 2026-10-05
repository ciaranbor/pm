//! The version this pm reports (`build.rs` sets it) and how two compare.
//! A build from source carries semver build metadata (`0.2.0+3.gabc1234`);
//! it compares as the release it was built from, and is a dev build.

use std::cmp::Ordering;

use semver::Version;

/// This binary's version.
pub const VERSION: &str = env!("PM_VERSION");

/// The target triple this binary was built for.
pub const TARGET: &str = env!("PM_TARGET");

/// Whether `version` is a build from source rather than a release.
pub fn is_dev(version: &str) -> bool {
    Version::parse(version).map_or(true, |v| !v.build.is_empty())
}

/// How `a` orders against `b`, ignoring build metadata; `None` if either
/// isn't semver. A leading `v`, as on a tag, is allowed.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    let parse = |s: &str| Version::parse(s.strip_prefix('v').unwrap_or(s)).ok();
    Some(parse(a)?.cmp_precedence(&parse(b)?))
}

/// `version` without its build metadata: the release it was built from.
pub fn base(version: &str) -> &str {
    version.split_once('+').map_or(version, |(base, _)| base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dev_build_compares_as_its_release_and_a_prerelease_precedes_it() {
        assert_eq!(
            compare("0.2.0+3.gabc.dirty", "v0.2.0"),
            Some(Ordering::Equal)
        );
        assert_eq!(compare("0.2.0+3.gabc", "v0.2.1"), Some(Ordering::Less));
        assert_eq!(compare("0.2.0-rc.1", "0.2.0"), Some(Ordering::Less));
        assert_eq!(compare("0.10.0", "0.9.0"), Some(Ordering::Greater));
        assert_eq!(compare("0.2.0", "latest"), None);
        assert!(is_dev("0.2.0+gabc1234"));
        assert!(!is_dev("0.2.0"));
        assert!(!is_dev("0.2.0-rc.1"));
    }
}
