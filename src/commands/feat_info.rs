use std::path::Path;

use crate::error::Result;
use crate::git;
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
    let main_repo = project_root.join("main");
    // Don't fail info display if remote lookup errors
    let remote = git::remote_tracking_branch(&main_repo, &state.branch).unwrap_or(None);
    lines.push(format!(
        "remote:      {}",
        remote.as_deref().unwrap_or("None")
    ));
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
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(
            &project_path,
            "alpha",
            None,
            Some("fix the widget"),
            None,
            false,
            server.name(),
        )
        .unwrap();

        let lines = feat_info(&project_path, "alpha").unwrap();
        let output = lines.join("\n");
        assert!(output.contains("name:        alpha"));
        assert!(output.contains("status:      wip"));
        assert!(output.contains("branch:      alpha"));
        assert!(output.contains("worktree:    alpha"));
        assert!(output.contains("remote:      None"));
        assert!(output.contains("context:     fix the widget"));
        assert!(output.contains("created:"));
        assert!(output.contains("last_active:"));
    }

    #[test]
    fn feat_info_shows_remote_when_upstream_set() {
        use std::process::Command;

        let dir = tempdir().unwrap();
        let server = TestServer::new();

        // Create a bare "remote" repo
        let bare_path = dir.path().join("remote.git");
        std::fs::create_dir_all(&bare_path).unwrap();
        Command::new("git")
            .args(["init", "--bare", &bare_path.to_string_lossy()])
            .output()
            .unwrap();

        // Init project (creates a real git repo at project_path/main)
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        let main_repo = project_path.join("main");

        // Add remote to the main git repo
        Command::new("git")
            .args([
                "-C",
                &main_repo.to_string_lossy(),
                "remote",
                "add",
                "origin",
                &bare_path.to_string_lossy(),
            ])
            .output()
            .unwrap();

        // Push main so remote has something
        Command::new("git")
            .args([
                "-C",
                &main_repo.to_string_lossy(),
                "push",
                "-u",
                "origin",
                "main",
            ])
            .output()
            .unwrap();

        // Create a feature (worktree at project_path/tracked)
        feat_new::feat_new(
            &project_path,
            "tracked",
            None,
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        // Push the feature branch to set up tracking
        let wt_path = project_path.join("tracked");
        Command::new("git")
            .args([
                "-C",
                &wt_path.to_string_lossy(),
                "push",
                "-u",
                "origin",
                "tracked",
            ])
            .output()
            .unwrap();

        let lines = feat_info(&project_path, "tracked").unwrap();
        let output = lines.join("\n");
        assert!(
            output.contains("remote:      origin/tracked"),
            "expected remote tracking branch, got:\n{output}"
        );
    }

    #[test]
    fn feat_info_nonexistent_returns_error() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();

        let result = feat_info(&project_path, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn feat_info_omits_empty_optional_fields() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, server.name()).unwrap();
        feat_new::feat_new(
            &project_path,
            "beta",
            None,
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        let lines = feat_info(&project_path, "beta").unwrap();
        let output = lines.join("\n");
        // base is always set (detected from CWD when not explicit)
        assert!(!output.contains("pr:"));
        assert!(!output.contains("context:"));
    }
}
