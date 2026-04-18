use std::path::{Path, PathBuf};

/// Convert an absolute path to a portable string by replacing $HOME with `~/`.
///
/// If the path doesn't start with $HOME, returns the path as-is.
pub fn to_portable(abs_path: &Path) -> String {
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
}
