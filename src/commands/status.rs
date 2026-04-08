use std::path::Path;

use crate::error::Result;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{commands::doctor, gh};

/// Show a project dashboard: name, root, features with statuses, PR info, and doctor issues.
pub fn status(project_root: &Path, tmux_server: Option<&str>) -> Result<Vec<String>> {
    let pm_dir = paths::pm_dir(project_root);
    let config = ProjectConfig::load(&pm_dir)?;
    let features_dir = paths::features_dir(project_root);
    let features = FeatureState::list(&features_dir)?;
    let main_repo = project_root.join("main");

    let mut lines = Vec::new();

    // Header
    lines.push(format!("Project:  {}", config.project.name));
    lines.push(format!("Root:     {}", project_root.to_string_lossy()));
    lines.push(format!("Features: {}", features.len()));

    // Feature list
    if !features.is_empty() {
        lines.push(String::new());

        // Calculate column widths
        let max_name = features.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
        let max_status = features
            .iter()
            .map(|(_, s)| s.status.to_string().len())
            .max()
            .unwrap_or(0);

        for (name, state) in &features {
            let status_str = state.status.to_string();
            let mut line = format!(
                "  {:<width_n$}  {:<width_s$}",
                name,
                status_str,
                width_n = max_name,
                width_s = max_status
            );

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

            lines.push(line);
        }
    }

    // Doctor issues
    let doctor_lines = doctor::doctor(project_root, false, tmux_server)?;
    let has_issues = !doctor_lines.is_empty()
        && !doctor_lines[0].contains("No features")
        && !doctor_lines[0].contains("all healthy");

    if has_issues {
        lines.push(String::new());
        lines.push("Issues:".to_string());
        // Skip the summary line (first), include per-feature issue lines
        for dl in &doctor_lines[1..] {
            if !dl.contains("— ok") {
                lines.push(dl.clone());
            }
        }
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
        let (project_path, _, _) = server.setup_project(dir.path());

        let lines = status(&project_path, server.name()).unwrap();
        assert!(lines[0].contains("Project:") && lines[0].contains(&server.scope("myapp")));
        assert!(lines[1].contains("Root:"));
        assert!(lines[2].contains("Features: 0"));
    }

    #[test]
    fn status_lists_features_with_status() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        feat_new::feat_new(
            &project_path,
            "alpha",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();
        feat_new::feat_new(
            &project_path,
            "beta",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        let lines = status(&project_path, server.name()).unwrap();
        assert!(lines[2].contains("Features: 2"));
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
    }

    #[test]
    fn status_shows_no_issues_section_when_healthy() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        let lines = status(&project_path, server.name()).unwrap();
        assert!(!lines.iter().any(|l| l.contains("Issues:")));
    }

    #[test]
    fn status_shows_issues_when_unhealthy() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        // Kill tmux session to create a doctor issue
        crate::tmux::kill_session(server.name(), &format!("{}/login", server.scope("myapp")))
            .unwrap();

        let lines = status(&project_path, server.name()).unwrap();
        assert!(lines.iter().any(|l| l.contains("Issues:")));
        assert!(lines.iter().any(|l| l.contains("tmux session")));
    }

    #[test]
    fn status_with_no_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        let lines = status(&project_path, server.name()).unwrap();
        assert!(lines[2].contains("Features: 0"));
        // No feature lines, no issues section
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn status_mixed_healthy_and_unhealthy_features() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        feat_new::feat_new(
            &project_path,
            "alpha",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();
        feat_new::feat_new(
            &project_path,
            "beta",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        // Break only beta's tmux session
        crate::tmux::kill_session(server.name(), &format!("{}/beta", server.scope("myapp")))
            .unwrap();

        let lines = status(&project_path, server.name()).unwrap();
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
        let (project_path, _, _) = server.setup_project(dir.path());
        feat_new::feat_new(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            None,
            server.name(),
        )
        .unwrap();

        // Manually set a PR number on the feature
        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.pr = "42".to_string();
        state.status = FeatureStatus::Review;
        state.save(&features_dir, "login").unwrap();

        let lines = status(&project_path, server.name()).unwrap();
        // gh CLI won't work in test, so we expect "status unknown"
        assert!(
            lines.iter().any(|l| l.contains("PR #42")),
            "expected PR #42 in output, got: {lines:?}"
        );
    }
}
