use std::path::Path;

use crate::error::Result;
use crate::state::project::ProjectEntry;

/// List all registered projects. Returns formatted lines for display.
pub fn list_projects(projects_dir: &Path) -> Result<Vec<String>> {
    let projects = ProjectEntry::list(projects_dir)?;

    let lines: Vec<String> = projects
        .iter()
        .map(|(name, entry)| format!("{name}\t{}", entry.root))
        .collect();

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn list_with_no_projects_returns_empty() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("registry");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let lines = list_projects(&projects_dir).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn list_shows_all_projects_with_roots() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();

        init::init(&dir.path().join("alpha"), &projects_dir, server.name()).unwrap();
        init::init(&dir.path().join("beta"), &projects_dir, server.name()).unwrap();

        let lines = list_projects(&projects_dir).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("alpha"));
        assert!(lines[1].contains("beta"));
    }
}
