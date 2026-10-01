use std::path::Path;

use crate::commands::{doctor, feat_list::lifecycle_token};
use crate::error::Result;
use crate::gh;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;

/// Show a project dashboard: name, root, features with statuses, PR info, and doctor issues.
pub fn status(
    project_root: &Path,
    projects_dir: &Path,
    tmux_server: Option<&str>,
) -> Result<Vec<String>> {
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let features_dir = paths::features_dir(project_root);
    let features = FeatureState::list(&features_dir)?;
    let main_repo = paths::main_worktree(project_root);

    let mut lines = Vec::new();

    // Header
    lines.push(format!("Project:  {}", config.project.name));
    lines.push(format!("Root:     {}", project_root.to_string_lossy()));
    lines.push(format!("Features: {}", features.len()));

    // Feature list
    if !features.is_empty() {
        lines.push(String::new());

        let max_name = features.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
        let max_progress = features
            .iter()
            .map(|(_, s)| s.progress.to_string().len())
            .max()
            .unwrap_or(0);

        for (name, state) in &features {
            let mut line = format!(
                "  {:<width_n$}  {:<width_p$}",
                name,
                state.progress.to_string(),
                width_n = max_name,
                width_p = max_progress
            );
            if let Some(lifecycle) = lifecycle_token(state.status) {
                line.push_str(&format!("  {lifecycle}"));
            }

            // PR info
            if !state.pr.is_empty() {
                match gh::pr_info(&main_repo, &state.pr) {
                    Ok(info) => {
                        let draft_label = if info.is_draft { ", draft" } else { "" };
                        let state_lower = info.state.to_lowercase();
                        line.push_str(&format!("  PR #{} ({state_lower}{draft_label})", state.pr));
                    }
                    Err(_) => {
                        line.push_str(&format!("  PR #{} (status unknown)", state.pr));
                    }
                }
            }

            lines.push(line.trim_end().to_string());
        }
    }

    let report = doctor::doctor(project_root, projects_dir, false, tmux_server)?;
    if report.issue_count() > 0 {
        lines.push(String::new());
        lines.push("Issues:".to_string());
        lines.extend(report.issue_lines().map(str::to_string));
        lines.extend(report.warnings().iter().cloned());
    }

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feat_new;
    use crate::state::feature::FeatureStatus;
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
        beta.progress = crate::state::feature::Progress::Ready;
        beta.status = FeatureStatus::Review;
        beta.save(&features_dir, "beta").unwrap();

        let lines = status(&project_path, &projects_dir, server.name()).unwrap();
        assert!(lines[2].contains("Features: 2"));
        let row = |name: &str| -> Vec<String> {
            let line = lines
                .iter()
                .find(|l| l.trim_start().starts_with(name))
                .unwrap();
            line.split_whitespace().map(str::to_string).collect()
        };
        assert_eq!(row("alpha"), ["alpha", "wip"]);
        assert_eq!(row("beta"), ["beta", "ready", "lifecycle:review"]);
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

    #[test]
    fn status_shows_pr_number_for_features_with_pr() {
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

        // Manually set a PR number on the feature
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.pr = "42".to_string();
        state.status = FeatureStatus::Review;
        state.save(&features_dir, "login").unwrap();

        let lines = status(&project_path, &projects_dir, server.name()).unwrap();
        // gh CLI won't work in test, so we expect "status unknown"
        assert!(
            lines.iter().any(|l| l.contains("PR #42")),
            "expected PR #42 in output, got: {lines:?}"
        );
    }
}
