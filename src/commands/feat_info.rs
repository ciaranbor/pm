use std::path::Path;

use crate::error::Result;
use crate::state::feature::FeatureState;
use crate::state::paths;

/// Display full details for a single feature.
/// Returns formatted lines for display.
pub fn feat_info(project_root: &Path, name: &str) -> Result<Vec<String>> {
    let features_dir = paths::features_dir(project_root);
    let state = FeatureState::load(&features_dir, name)?;

    let mut lines = Vec::new();
    lines.push(format!("name:        {name}"));
    lines.push(format!("status:      {}", state.status));
    lines.push(format!("branch:      {}", state.branch));
    lines.push(format!("worktree:    {}", state.worktree));
    if !state.base.is_empty() {
        lines.push(format!("base:        {}", state.base));
    }
    if !state.pr.is_empty() {
        lines.push(format!("pr:          {}", state.pr));
    }
    if !state.context.is_empty() {
        lines.push(format!("context:     {}", state.context));
    }
    lines.push(format!(
        "created:     {}",
        state.created.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    lines.push(format!(
        "last_active: {}",
        state.last_active.format("%Y-%m-%d %H:%M:%S UTC")
    ));

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn feat_info_shows_all_fields() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(
            &project_path,
            "alpha",
            Some("fix the widget"),
            server.name(),
        )
        .unwrap();

        let lines = feat_info(&project_path, "alpha").unwrap();
        let output = lines.join("\n");
        assert!(output.contains("name:        alpha"));
        assert!(output.contains("status:      wip"));
        assert!(output.contains("branch:      alpha"));
        assert!(output.contains("worktree:    alpha"));
        assert!(output.contains("context:     fix the widget"));
        assert!(output.contains("created:"));
        assert!(output.contains("last_active:"));
    }

    #[test]
    fn feat_info_nonexistent_returns_error() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        let result = feat_info(&project_path, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn feat_info_omits_empty_optional_fields() {
        let dir = tempdir().unwrap();
        let project_path = dir.path().join("myapp");
        let projects_dir = dir.path().join("registry");
        let server = TestServer::new();
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(&project_path, "beta", None, server.name()).unwrap();

        let lines = feat_info(&project_path, "beta").unwrap();
        let output = lines.join("\n");
        assert!(!output.contains("base:"));
        assert!(!output.contains("pr:"));
        assert!(!output.contains("context:"));
    }
}
