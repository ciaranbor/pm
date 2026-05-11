use std::path::{Path, PathBuf};

/// Convert an absolute path to a portable string by replacing $HOME with `~/`.
///
/// The input path **must** be absolute. Relative paths are not portable and
/// would silently corrupt the registry (resolved against the caller's CWD on
/// every load). Callers that hold a possibly-relative path should run
/// [`absolutize`] first.
///
/// Panics in debug builds if the path is relative. Release builds silently
/// fall through and return the path verbatim — callers that iterate stored
/// registry entries should use [`is_portable`] to guard against legacy bad
/// entries before calling this function.
pub fn to_portable(abs_path: &Path) -> String {
    debug_assert!(
        abs_path.is_absolute(),
        "to_portable() requires an absolute path, got: {abs_path:?}"
    );
    if let Some(home) = home_dir()
        && let Ok(suffix) = abs_path.strip_prefix(&home)
    {
        return format!("~/{}", suffix.display());
    }
    abs_path.to_string_lossy().to_string()
}

/// Resolve a portable path string back to an absolute `PathBuf`.
///
/// - If the string starts with `~/`, expands it to `$HOME/…`
/// - Otherwise, returns it as-is (backward compat with existing absolute paths)
pub fn resolve(portable: &str) -> PathBuf {
    if let Some(rest) = portable.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(portable)
}

/// Returns `true` if `s` is a portable registry path: either absolute (`/…`)
/// or starts with `~/`. A bare relative path like `exo-bench` is not portable.
pub fn is_portable(s: &str) -> bool {
    s.starts_with('/') || s.starts_with("~/")
}

/// Convert a possibly-relative path to an absolute `PathBuf` by joining
/// against the current working directory.
///
/// Note: this does not resolve symlinks or `..` components — use
/// [`Path::canonicalize`] for that (which requires the path to exist).
pub fn absolutize(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_portable_strips_home() {
        let home = home_dir().unwrap();
        let abs = home.join("Projects/myapp");
        let portable = to_portable(&abs);
        assert_eq!(portable, "~/Projects/myapp");
    }

    #[test]
    fn to_portable_non_home_path_unchanged() {
        let path = Path::new("/tmp/something");
        assert_eq!(to_portable(path), "/tmp/something");
    }

    #[test]
    fn resolve_expands_tilde() {
        let home = home_dir().unwrap();
        let resolved = resolve("~/Projects/myapp");
        assert_eq!(resolved, home.join("Projects/myapp"));
    }

    #[test]
    fn resolve_absolute_path_unchanged() {
        let resolved = resolve("/tmp/something");
        assert_eq!(resolved, PathBuf::from("/tmp/something"));
    }

    #[test]
    fn roundtrip() {
        let home = home_dir().unwrap();
        let original = home.join("Projects/test");
        let portable = to_portable(&original);
        let resolved = resolve(&portable);
        assert_eq!(resolved, original);
    }

    #[test]
    fn is_portable_accepts_absolute_paths() {
        assert!(is_portable("/home/user/projects/app"));
        assert!(is_portable("/tmp/x"));
    }

    #[test]
    fn is_portable_accepts_tilde_paths() {
        assert!(is_portable("~/Projects/app"));
        assert!(is_portable("~/"));
    }

    #[test]
    fn is_portable_rejects_relative_paths() {
        assert!(!is_portable("exo-bench"));
        assert!(!is_portable("./app"));
        assert!(!is_portable("../app"));
        assert!(!is_portable(""));
    }

    #[test]
    fn absolutize_passes_absolute_through() {
        let abs = PathBuf::from("/tmp/absolute");
        assert_eq!(absolutize(&abs).unwrap(), abs);
    }

    #[test]
    fn absolutize_joins_relative_with_cwd() {
        // We can't reliably set CWD in parallel tests, but we can verify
        // that the result is absolute and ends with the original path.
        let rel = Path::new("some-relative-thing");
        let result = absolutize(rel).unwrap();
        assert!(
            result.is_absolute(),
            "result should be absolute: {result:?}"
        );
        assert!(result.ends_with("some-relative-thing"));
    }

    #[test]
    #[should_panic(expected = "requires an absolute path")]
    #[cfg(debug_assertions)]
    fn to_portable_panics_on_relative_in_debug() {
        // Documents the contract: relative paths are a programming error.
        // The validation also runs at save time (ProjectEntry::save).
        to_portable(Path::new("relative-thing"));
    }
}
