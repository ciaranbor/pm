use std::path::Path;

use crate::error::Result;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectEntry;

/// The PR-derived lifecycle status as a listing token, omitted while it is
/// the unremarkable `wip`.
pub(crate) fn lifecycle_token(status: FeatureStatus) -> Option<String> {
    (status != FeatureStatus::Wip).then(|| format!("lifecycle:{status}"))
}

/// List all features for the project at the given root.
/// Returns formatted lines for display.
pub fn feat_list(project_root: &Path) -> Result<Vec<String>> {
    let features_dir = paths::features_dir(project_root);
    let features = FeatureState::list(&features_dir)?;

    if features.is_empty() {
        return Ok(Vec::new());
    }

    let name_w = features.iter().map(|(n, _)| n.len()).max().unwrap().max(4);
    let progress_w = features
        .iter()
        .map(|(_, s)| s.progress.to_string().len())
        .max()
        .unwrap();

    let mut lines = Vec::new();

    for (name, state) in &features {
        let mut line = format!("{:<name_w$}  {:<progress_w$}", name, state.progress);
        if let Some(lifecycle) = lifecycle_token(state.status) {
            line.push_str(&format!("  {lifecycle}"));
        }
        if !state.branch.is_empty() && state.branch != *name {
            line.push_str(&format!("  branch:{}", state.branch));
        }
        if !state.base.is_empty() {
            line.push_str(&format!("  base:{}", state.base));
        }
        if !state.pr.is_empty() {
            line.push_str(&format!("  pr:{}", state.pr));
        }
        lines.push(line.trim_end().to_string());
    }

    Ok(lines)
}

/// [`feat_list`] for every registered project, each project's rows under a
/// header. A project whose state can't be read gets a one-line note
/// instead, so one broken entry doesn't hide the rest.
pub fn feat_list_all(projects_dir: &Path) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for (name, entry) in ProjectEntry::list(projects_dir)? {
        let root = entry.root_path();
        let listed = if paths::pm_dir(&root).is_dir() {
            feat_list(&root).map_err(|e| e.to_string())
        } else {
            Err(format!("no pm project at {}", root.display()))
        };
        match listed {
            Ok(rows) if rows.is_empty() => lines.push(format!("{name}: no features")),
            Ok(rows) => {
                lines.push(format!("{name}:"));
                lines.extend(rows.into_iter().map(|r| format!("  {r}")));
            }
            Err(e) => lines.push(format!("{name}: skipped ({e})")),
        }
    }
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
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        let lines = feat_list(&project_path).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn feat_list_shows_annotations_for_enriched_fields() {
        use crate::state::feature::{FeatureState, FeatureStatus};
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        // Create a feature with non-default branch, base, and pr
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState {
            status: FeatureStatus::Review,
            branch: "feature/login-v2".to_string(),
            worktree: "login".to_string(),
            base: "develop".to_string(),
            pr: "https://github.com/org/repo/pull/42".to_string(),
            context: String::new(),
            workflow: None,
            created: Utc::now(),
            last_active: Utc::now(),
            progress: crate::state::feature::Progress::Blocked,
            blocked_reason: None,
            blocked_by: None,
        };
        state.save(&features_dir, "login").unwrap();

        let lines = feat_list(&project_path).unwrap();
        assert_eq!(lines.len(), 1);
        let columns: Vec<&str> = lines[0].split_whitespace().collect();
        assert_eq!(columns[..3], ["login", "blocked", "lifecycle:review"]);
        assert!(lines[0].contains("branch:feature/login-v2"));
        assert!(lines[0].contains("base:develop"));
        assert!(lines[0].contains("pr:https://github.com/org/repo/pull/42"));
    }

    #[test]
    fn feat_list_omits_branch_when_same_as_name() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "alpha",
            server.name(),
        ))
        .unwrap();

        let lines = feat_list(&project_path).unwrap();
        assert!(!lines[0].contains("branch:"));
    }

    #[test]
    fn feat_list_shows_all_features_with_status() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
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

        let lines = feat_list(&project_path).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "alpha  wip  base:main");
        assert_eq!(lines[1], "beta   wip  base:main");
    }

    #[test]
    fn feat_list_all_groups_features_by_project_and_skips_a_missing_one() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let projects_dir = dir.path().join("registry");
        let mut roots = Vec::new();
        for (project, feature) in [("alpha", "login"), ("beta", "search")] {
            let root = dir.path().join(server.scope(project));
            init::init(&root, &projects_dir, None, server.name()).unwrap();
            feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
                &root,
                &projects_dir,
                feature,
                server.name(),
            ))
            .unwrap();
            roots.push(root);
        }
        let gone = dir.path().join(server.scope("gone"));
        init::init(&gone, &projects_dir, None, server.name()).unwrap();
        std::fs::remove_dir_all(&gone).unwrap();

        let lines = feat_list_all(&projects_dir).unwrap();

        assert_eq!(
            lines,
            vec![
                format!("{}:", server.scope("alpha")),
                "  login  wip  base:main".to_string(),
                format!("{}:", server.scope("beta")),
                "  search  wip  base:main".to_string(),
                format!(
                    "{}: skipped (no pm project at {})",
                    server.scope("gone"),
                    gone.display()
                ),
            ]
        );
    }
}
