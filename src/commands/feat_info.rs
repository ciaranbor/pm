use std::path::Path;

use crate::commands::feat_sync::sync_one;
use crate::error::Result;
use crate::git;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;

/// Human-readable label for a feature status derived from PR state.
fn pr_status_label(status: FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::Merged => "merged",
        FeatureStatus::Stale => "closed",
        FeatureStatus::Wip => "draft",
        FeatureStatus::Approved => "approved",
        FeatureStatus::Review => "open",
        FeatureStatus::Initializing => "unknown",
    }
}

/// Display full details for a single feature.
/// Returns formatted lines for display.
pub fn feat_info(project_root: &Path, name: &str) -> Result<Vec<String>> {
    let features_dir = paths::features_dir(project_root);
    let mut state = FeatureState::load(&features_dir, name)?;

    let main_repo = project_root.join("main");

    // If PR is linked, query GitHub and sync local status
    let mut pr_status_line = None;
    if !state.pr.is_empty() {
        match sync_one(&mut state, &features_dir, name, &main_repo) {
            Ok(()) => {
                pr_status_line = Some(format!("pr_status:   {}", pr_status_label(state.status)));
            }
            Err(_) => {
                pr_status_line = Some("pr_status:   (query failed)".to_string());
            }
        }
    }

    let mut lines = Vec::new();
    lines.push(format!("name:        {name}"));
    lines.push(format!("status:      {}", state.status));
    lines.push(format!("branch:      {}", state.branch));
    lines.push(format!("worktree:    {}", state.worktree));
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
        lines.push(format!("pr:          #{}", state.pr));
        if let Some(line) = pr_status_line {
            lines.push(line);
        }
    }

    // Show branch divergence from base
    let base = state.base_or_default();
    match git::branch_divergence(&main_repo, &state.branch, base) {
        Ok(div) => {
            lines.push(format!("divergence:  {} {base}", div));
        }
        Err(_) => {
            // Silently skip if divergence check fails (e.g. base branch doesn't exist)
        }
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
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams {
            project_root: &project_path,
            name: "alpha",
            name_override: None,
            context: Some("fix the widget"),
            base: None,
            edit: false,
            agent_override: None,
            tmux_server: server.name(),
        })
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
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

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
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "tracked",
            server.name(),
        ))
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
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        let result = feat_info(&project_path, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn feat_info_omits_empty_optional_fields() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "beta",
            server.name(),
        ))
        .unwrap();

        let lines = feat_info(&project_path, "beta").unwrap();
        let output = lines.join("\n");
        // base is always set (detected from CWD when not explicit)
        assert!(!output.contains("pr:"));
        assert!(!output.contains("context:"));
    }

    #[test]
    fn feat_info_shows_divergence() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "diverge",
            server.name(),
        ))
        .unwrap();

        // Add a commit on the feature branch
        let wt_path = project_path.join("diverge");
        std::fs::write(wt_path.join("feat.txt"), "content").unwrap();
        git::stage_file(&wt_path, "feat.txt").unwrap();
        git::commit(&wt_path, "feature commit").unwrap();

        let lines = feat_info(&project_path, "diverge").unwrap();
        let output = lines.join("\n");
        assert!(
            output.contains("divergence:  1 commit ahead main"),
            "expected divergence info, got:\n{output}"
        );
    }

    #[test]
    fn feat_info_shows_up_to_date_divergence() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "synced",
            server.name(),
        ))
        .unwrap();

        let lines = feat_info(&project_path, "synced").unwrap();
        let output = lines.join("\n");
        assert!(
            output.contains("divergence:  up to date main"),
            "expected up to date, got:\n{output}"
        );
    }

    #[test]
    fn feat_info_shows_behind_divergence() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "behind",
            server.name(),
        ))
        .unwrap();

        // Add a commit on main (the feature is now behind)
        let main_repo = project_path.join("main");
        std::fs::write(main_repo.join("main.txt"), "content").unwrap();
        git::stage_file(&main_repo, "main.txt").unwrap();
        git::commit(&main_repo, "main commit").unwrap();

        let lines = feat_info(&project_path, "behind").unwrap();
        let output = lines.join("\n");
        assert!(
            output.contains("divergence:  1 commit behind main"),
            "expected behind info, got:\n{output}"
        );
    }

    #[test]
    fn feat_info_shows_ahead_and_behind() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            "both",
            server.name(),
        ))
        .unwrap();

        // Add a commit on the feature branch
        let wt_path = project_path.join("both");
        std::fs::write(wt_path.join("feat.txt"), "content").unwrap();
        git::stage_file(&wt_path, "feat.txt").unwrap();
        git::commit(&wt_path, "feature commit").unwrap();

        // Add a commit on main
        let main_repo = project_path.join("main");
        std::fs::write(main_repo.join("main.txt"), "content").unwrap();
        git::stage_file(&main_repo, "main.txt").unwrap();
        git::commit(&main_repo, "main commit").unwrap();

        let lines = feat_info(&project_path, "both").unwrap();
        let output = lines.join("\n");
        assert!(
            output.contains("divergence:  1 commit ahead, 1 behind main"),
            "expected ahead and behind, got:\n{output}"
        );
    }

    #[test]
    fn pr_status_label_maps_correctly() {
        use crate::state::feature::FeatureStatus;

        assert_eq!(pr_status_label(FeatureStatus::Review), "open");
        assert_eq!(pr_status_label(FeatureStatus::Wip), "draft");
        assert_eq!(pr_status_label(FeatureStatus::Approved), "approved");
        assert_eq!(pr_status_label(FeatureStatus::Merged), "merged");
        assert_eq!(pr_status_label(FeatureStatus::Stale), "closed");
        assert_eq!(pr_status_label(FeatureStatus::Initializing), "unknown");
    }
}
