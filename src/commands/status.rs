//! `pm status`: project health — the header, the project's
//! [`super::attention`] rows, then doctor's issues and warnings. PR state is what `pm
//! feat sync` last recorded: nothing here calls `gh`, so PR drift is left to
//! `pm doctor`.

use std::path::Path;

use crate::commands::{attention, doctor, feat_status_view};
use crate::error::Result;

/// Show a project dashboard: name, root, what each feature needs, and
/// doctor's issues and warnings.
pub fn status(
    project_root: &Path,
    projects_dir: &Path,
    tmux_server: Option<&str>,
) -> Result<Vec<String>> {
    let snapshot = attention::project(project_root, tmux_server)?;

    let mut lines = vec![
        format!("Project:  {}", snapshot.projects[0].name),
        format!("Root:     {}", project_root.to_string_lossy()),
        format!("Features: {}", snapshot.features.len()),
    ];
    let rows = feat_status_view::rows(&snapshot.features, &snapshot.projects, false);
    if !rows.is_empty() {
        lines.push(String::new());
        lines.extend(rows.into_iter().map(|row| format!("  {row}")));
    }

    let report = doctor::offline(project_root, projects_dir, tmux_server)?;
    if report.issue_count() > 0 {
        lines.push(String::new());
        lines.push("Issues:".to_string());
        lines.extend(report.issue_lines().map(str::to_string));
    }
    if !report.warnings().is_empty() {
        lines.push(String::new());
        lines.push("Warnings:".to_string());
        lines.extend(report.warnings().iter().map(|w| format!("  {w}")));
    }

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feat_new;
    use crate::state::feature::{FeatureState, FeatureStatus};
    use crate::state::paths;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn status_shows_project_info() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());

        let lines = status(&project_path, &projects_dir, server.name()).unwrap();
        assert!(lines[0].contains("Project:") && lines[0].contains(&server.scope("myapp")));
        assert!(lines[1].contains("Root:"));
        assert!(lines[2].contains("Features: 0"));
    }

    #[test]
    fn status_lists_features_with_status() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "alpha",
            server.name(),
        ))
        .unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "beta",
            server.name(),
        ))
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let mut beta = FeatureState::load(&features_dir, "beta").unwrap();
        beta.status = FeatureStatus::Approved;
        beta.pr = "42".to_string();
        beta.save(&features_dir, "beta").unwrap();

        let lines = status(&project_path, &projects_dir, server.name()).unwrap();
        assert!(lines[2].contains("Features: 2"));
        let rows: Vec<Vec<&str>> = lines[4..6]
            .iter()
            .map(|l| l.split_whitespace().collect())
            .collect();
        assert_eq!(rows[0], ["beta", "ready", "PR", "approved"]);
        assert_eq!(rows[1], ["alpha", "wip"]);
    }

    #[test]
    fn status_shows_no_issues_section_when_healthy() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();

        let lines = status(&project_path, &projects_dir, server.name()).unwrap();
        assert!(!lines.iter().any(|l| l.contains("Issues:")));
    }

    #[test]
    fn status_shows_issues_when_unhealthy() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();

        // Kill tmux session to create a doctor issue
        crate::tmux::kill_session(server.name(), &format!("{}/login", server.scope("myapp")))
            .unwrap();

        let lines = status(&project_path, &projects_dir, server.name()).unwrap();
        assert!(lines.iter().any(|l| l.contains("Issues:")));
        assert!(lines.iter().any(|l| l.contains("tmux session")));
    }

    #[test]
    fn status_shows_warnings_without_issues() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        std::fs::write(projects_dir.join("bad.toml"), "root = ").unwrap();

        let lines = status(&project_path, &projects_dir, server.name()).unwrap();
        assert!(!lines.iter().any(|l| l.contains("Issues:")), "{lines:?}");
        let warnings = lines.iter().position(|l| l == "Warnings:").unwrap();
        assert!(lines[warnings + 1].contains("'bad'"), "{lines:?}");
    }

    #[test]
    fn status_reports_a_feature_paused_mid_rebase() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "login",
            server.name(),
        ))
        .unwrap();
        TestServer::add_feature_commit(&project_path, "login");
        TestServer::pause_rebase(&project_path.join("login"), "main");

        let lines = status(&project_path, &projects_dir, server.name()).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("login") && l.contains("rebase in progress")),
            "{lines:?}"
        );
    }

    #[test]
    fn status_with_no_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());

        let lines = status(&project_path, &projects_dir, server.name()).unwrap();
        assert!(lines[2].contains("Features: 0"));
        // No feature lines, no issues section
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn status_mixed_healthy_and_unhealthy_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "alpha",
            server.name(),
        ))
        .unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "beta",
            server.name(),
        ))
        .unwrap();

        // Break only beta's tmux session
        crate::tmux::kill_session(server.name(), &format!("{}/beta", server.scope("myapp")))
            .unwrap();

        let lines = status(&project_path, &projects_dir, server.name()).unwrap();
        // Both features listed
        assert!(
            lines
                .iter()
                .any(|l| l.contains("alpha") && l.contains("wip"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("beta") && l.contains("wip"))
        );
        // Issues section present, only beta's issue shown
        assert!(lines.iter().any(|l| l.contains("Issues:")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("beta") && l.contains("tmux session"))
        );
        // alpha's "ok" line should NOT appear in the issues section
        let issues_start = lines.iter().position(|l| l.contains("Issues:")).unwrap();
        let issue_lines = &lines[issues_start + 1..];
        assert!(
            !issue_lines.iter().any(|l| l.contains("alpha")),
            "alpha should not appear in issues section, got: {issue_lines:?}"
        );
    }
}
