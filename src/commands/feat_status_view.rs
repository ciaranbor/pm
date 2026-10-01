//! Bare `pm feat status`: what needs the user's attention, for one feature
//! or, from `main`, for every feature in the project — why a blocked team
//! is waiting and what a ready one's summary says. `pm feat list` is the
//! inventory (branch, base, PR); this view omits those.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;

/// Summary lines shown in a single feature's view.
const SUMMARY_HEAD_LINES: usize = 10;

/// One feature: its progress, reason, last activity and summary head.
pub fn feature(project_root: &Path, name: &str) -> Result<Vec<String>> {
    let state = FeatureState::load(&paths::features_dir(project_root), name)?;
    let mut lines = vec![format!(
        "{name}  {}  (last active {})",
        state.progress,
        age(state.last_active, Utc::now())
    )];
    if let Some(reason) = reason(&state) {
        lines.push(format!("blocked on: {reason}"));
    }
    let path = paths::summary_path(project_root, name);
    match std::fs::read_to_string(&path) {
        Ok(summary) if !summary.trim().is_empty() => {
            lines.push(format!("summary ({}):", path.display()));
            let body: Vec<&str> = summary.trim_end().lines().collect();
            lines.extend(
                body.iter()
                    .take(SUMMARY_HEAD_LINES)
                    .map(|l| format!("  {l}").trim_end().to_string()),
            );
            if body.len() > SUMMARY_HEAD_LINES {
                lines.push(format!(
                    "  … {} more lines (`pm feat summary show {name}`)",
                    body.len() - SUMMARY_HEAD_LINES
                ));
            }
        }
        _ => lines.push("no summary".to_string()),
    }
    Ok(lines)
}

/// Every feature in the project, one row each: name, progress, and the
/// blocked reason or the ready summary's first line.
pub fn project(project_root: &Path) -> Result<Vec<String>> {
    let features = FeatureState::list(&paths::features_dir(project_root))?;
    let name_w = features.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let progress_w = "blocked".len();
    Ok(features
        .iter()
        .map(|(name, state)| {
            let note = match state.progress {
                Progress::Blocked => reason(state).map(str::to_string),
                Progress::Ready => first_line(project_root, name),
                Progress::Wip => None,
            };
            format!(
                "{name:<name_w$}  {:<progress_w$}  {}",
                state.progress,
                note.unwrap_or_default()
            )
            .trim_end()
            .to_string()
        })
        .collect())
}

fn reason(state: &FeatureState) -> Option<&str> {
    (state.progress == Progress::Blocked)
        .then_some(state.blocked_reason.as_deref())
        .flatten()
}

/// The summary's first line of text: summaries open with a title heading
/// that only repeats the feature name.
fn first_line(project_root: &Path, name: &str) -> Option<String> {
    let summary = std::fs::read_to_string(paths::summary_path(project_root, name)).ok()?;
    summary
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
}

fn age(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_status::feat_status, feat_summary};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn write_summary(project: &Path, name: &str, body: &str) {
        std::fs::write(feat_summary::path(project, name).unwrap(), body).unwrap();
    }

    fn add_feature(project: &Path, name: &str) {
        let features_dir = paths::features_dir(project);
        let mut state = FeatureState::load(&features_dir, "login").unwrap();
        state.branch = name.to_string();
        state.worktree = name.to_string();
        state.save(&features_dir, name).unwrap();
    }

    #[test]
    fn project_view_shows_the_blocked_reason_and_the_ready_summary() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        add_feature(&project, "auth");
        add_feature(&project, "search");
        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            None,
        )
        .unwrap();
        write_summary(&project, "auth", "# auth\n\nAdds OAuth login\n\n## More\n");
        feat_status(&project, "auth", Progress::Ready, None, None).unwrap();

        let lines = super::project(&project).unwrap();

        assert_eq!(
            lines,
            vec![
                "auth    ready    Adds OAuth login",
                "login   blocked  which DB?",
                "search  wip",
            ]
        );
    }

    #[test]
    fn feature_view_shows_the_reason_and_the_summary_head() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            None,
        )
        .unwrap();
        assert_eq!(feature(&project, "login").unwrap()[2], "no summary");

        let body: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        write_summary(&project, "login", &body);
        let lines = feature(&project, "login").unwrap();

        assert!(
            lines[0].starts_with("login  blocked  (last active "),
            "{lines:?}"
        );
        assert_eq!(lines[1], "blocked on: which DB?");
        assert_eq!(lines[3], "  line 1");
        assert_eq!(lines[12], "  line 10");
        assert!(lines[13].starts_with("  … 2 more lines"), "{lines:?}");
        assert_eq!(lines.len(), 14);
    }

    #[test]
    fn age_rounds_down_to_the_largest_unit() {
        let now = Utc::now();
        let ago = |secs| age(now - chrono::Duration::seconds(secs), now);
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(150), "2m ago");
        assert_eq!(ago(7300), "2h ago");
        assert_eq!(ago(200_000), "2d ago");
    }
}
