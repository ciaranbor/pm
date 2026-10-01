//! Bare `pm feat status`: what needs the user's attention, for one feature
//! or, from `main`, the [`super::attention`] snapshot of every feature in
//! the project (all projects with `--all`). `pm feat list` is the inventory
//! (branch, base, PR); this view omits those.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;

use super::attention::{self, AttentionKind, FeatureSnapshot};

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
        match &state.blocked_by {
            Some(agent) => lines.push(format!("blocked on: {reason} ({agent})")),
            None => lines.push(format!("blocked on: {reason}")),
        }
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

/// Every feature of the project at `project_root`, or only `name`: rows,
/// or the snapshot as JSON.
pub fn project(
    project_root: &Path,
    name: Option<&str>,
    json: bool,
    tmux_server: Option<&str>,
) -> Result<Vec<String>> {
    let mut snapshot = attention::project(project_root, tmux_server)?;
    if let Some(name) = name {
        snapshot.features.retain(|f| f.name == name);
    }
    if json {
        return Ok(vec![serde_json::to_string_pretty(&snapshot)?]);
    }
    Ok(rows(&snapshot.features, false))
}

/// Every feature of every registered project, with a line for each project
/// skipped: rows, or the snapshot as JSON.
pub fn all(projects_dir: &Path, json: bool, tmux_server: Option<&str>) -> Result<Vec<String>> {
    let snapshot = attention::all(projects_dir, tmux_server)?;
    if json {
        return Ok(vec![serde_json::to_string_pretty(&snapshot)?]);
    }
    let mut lines = rows(&snapshot.features, true);
    lines.extend(snapshot.projects.iter().filter_map(|p| {
        p.skipped
            .as_ref()
            .map(|why| format!("{}: skipped ({why})", p.name))
    }));
    Ok(lines)
}

/// One row per feature, in the snapshot's order: name (with its project
/// when `with_project`), what it needs (its progress when nothing), its
/// agents, and the detail.
pub fn rows(features: &[FeatureSnapshot], with_project: bool) -> Vec<String> {
    let name = |f: &FeatureSnapshot| {
        if with_project {
            format!("{}/{}", f.project, f.name)
        } else {
            f.name.clone()
        }
    };
    let label = |f: &FeatureSnapshot| match f.attention.kind {
        AttentionKind::None => f.progress.to_string(),
        kind => kind.to_string(),
    };
    let rendered: Vec<[String; 4]> = features
        .iter()
        .map(|f| [name(f), label(f), agents(f), detail(f)])
        .collect();
    let width = |col: usize| rendered.iter().map(|r| r[col].len()).max().unwrap_or(0);
    let (name_w, label_w, agents_w) = (width(0), width(1), width(2));
    rendered
        .iter()
        .map(|[name, label, agents, detail]| {
            format!("{name:<name_w$}  {label:<label_w$}  {agents:<agents_w$}  {detail}")
                .trim_end()
                .to_string()
        })
        .collect()
}

/// `agent:state` for each agent, `+N` for its unread messages; `no session`
/// when the feature's session is closed.
fn agents(feature: &FeatureSnapshot) -> String {
    if !feature.session_exists {
        return "no session".to_string();
    }
    feature
        .agents
        .iter()
        .map(|a| match a.unread {
            0 => format!("{}:{}", a.name, a.state),
            n => format!("{}:{}+{n}", a.name, a.state),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn detail(feature: &FeatureSnapshot) -> String {
    let attention = &feature.attention;
    match (attention.kind, &attention.agent, &attention.detail) {
        (AttentionKind::Blocked, Some(agent), Some(reason)) => format!("{agent}: {reason}"),
        (AttentionKind::Blocked, Some(agent), None) => format!("{agent}: (no reason given)"),
        (AttentionKind::Stalled, _, _) => "every agent idle, no unread messages".to_string(),
        (_, _, Some(detail)) => detail.clone(),
        _ => String::new(),
    }
}

fn reason(state: &FeatureState) -> Option<&str> {
    (state.progress == Progress::Blocked)
        .then_some(state.blocked_reason.as_deref())
        .flatten()
}

/// The summary's first line of text: summaries open with a title heading
/// that only repeats the feature name.
pub(crate) fn first_line(project_root: &Path, name: &str) -> Option<String> {
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
    fn project_view_puts_the_most_urgent_first_with_who_is_blocked() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        add_feature(&project, "auth");
        add_feature(&project, "search");
        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            Some("implementer"),
        )
        .unwrap();
        write_summary(&project, "auth", "# auth\n\nAdds OAuth login\n\n## More\n");
        feat_status(&project, "auth", Progress::Ready, None, None).unwrap();

        let lines = super::project(&project, None, false, server.name()).unwrap();

        assert_eq!(
            lines,
            vec![
                "login   blocked  no session  implementer: which DB?",
                "auth    ready    no session  Adds OAuth login",
                "search  wip      no session",
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
            Some("implementer"),
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
        assert_eq!(lines[1], "blocked on: which DB? (implementer)");
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
