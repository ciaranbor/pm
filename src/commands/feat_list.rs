use std::path::Path;

use crate::error::Result;
use crate::state::feature::FeatureState;
use crate::state::paths;

/// List all features for the project at the given root.
/// Returns formatted lines for display.
pub fn feat_list(project_root: &Path) -> Result<Vec<String>> {
    let features_dir = paths::features_dir(project_root);
    let features = FeatureState::list(&features_dir)?;

    let lines: Vec<String> = features
        .iter()
        .map(|(name, state)| format!("{name}\t{}", state.status))
        .collect();

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn feat_list_with_no_features_returns_empty() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir).unwrap();

        let lines = feat_list(&project_path).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn feat_list_shows_all_features_with_status() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir).unwrap();

        let server = TestServer::new();
        feat_new::feat_new(&project_path, "alpha", server.name()).unwrap();
        feat_new::feat_new(&project_path, "beta", server.name()).unwrap();

        let lines = feat_list(&project_path).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("alpha"));
        assert!(lines[0].contains("wip"));
        assert!(lines[1].contains("beta"));
        assert!(lines[1].contains("wip"));
    }
}
