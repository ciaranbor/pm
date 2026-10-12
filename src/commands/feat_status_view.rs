//! Bare `pm feat status`: what needs the user's attention, for one feature
//! or, from `main`, the [`super::attention`] snapshot of every feature in
//! the project (all projects with `--all`), with a row for a main scope
//! that needs it too. Each agent's latest line follows its scope's row.
//! `pm feat list` is the inventory (branch, base, PR); this view omits those.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;
use crate::state::runtime::{self, Latest};
use crate::state::scope::{self, TeamStatus};

use super::attention::{
    self, Activity, AgentSnapshot, Attention, AttentionKind, FeatureSnapshot, ProjectSnapshot,
};

/// Summary lines shown in a single feature's view.
const SUMMARY_HEAD_LINES: usize = 10;

/// One scope: its progress, reason, last activity and, for a feature, its
/// summary head.
pub fn feature(project_root: &Path, name: &str) -> Result<Vec<String>> {
    let team = TeamStatus::load(project_root, name)?;
    let recorded = match name {
        scope::MAIN => None,
        _ => Some(FeatureState::load(&paths::features_dir(project_root), name)?.last_active),
    };
    let mut head = format!("{name}  {}", team.progress);
    if let Some(active) = runtime::scope_last_activity(project_root, name).or(recorded) {
        head.push_str(&format!("  (last active {})", age(active, Utc::now())));
    }
    let mut lines = vec![head];
    if let Some(reason) = team.reason() {
        match team.agent() {
            Some(agent) => lines.push(format!("blocked on: {reason} ({agent})")),
            None => lines.push(format!("blocked on: {reason}")),
        }
    }
    let latest = attention::scope_latest(project_root, name)?;
    if !latest.is_empty() {
        lines.push("latest:".to_string());
        lines.extend(
            latest
                .iter()
                .map(|(agent, latest)| latest_line("  ", agent, latest)),
        );
    }
    if name == scope::MAIN {
        return Ok(lines);
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
    let mains = if name.is_some() {
        &[][..]
    } else {
        &snapshot.projects[..]
    };
    Ok(rows(&snapshot.features, mains, false))
}

/// Every feature of every registered project, with a line for each project
/// skipped: rows, or the snapshot as JSON.
pub fn all(projects_dir: &Path, json: bool, tmux_server: Option<&str>) -> Result<Vec<String>> {
    let snapshot = attention::all(projects_dir, tmux_server)?;
    if json {
        return Ok(vec![serde_json::to_string_pretty(&snapshot)?]);
    }
    let mut lines = rows(&snapshot.features, &snapshot.projects, true);
    lines.extend(snapshot.projects.iter().filter_map(|p| {
        p.skipped
            .as_ref()
            .map(|why| format!("{}: skipped ({why})", p.name))
    }));
    Ok(lines)
}

/// One row per feature, in the snapshot's order, and one for each of
/// `projects`' main scopes that needs attention, ranked among them: name
/// (with its project when `with_project`), what it needs (its progress when
/// nothing), its agents, how long it has waited on background work or been
/// quiet, and the detail.
pub fn rows(
    features: &[FeatureSnapshot],
    projects: &[ProjectSnapshot],
    with_project: bool,
) -> Vec<String> {
    let now = Utc::now();
    let name = |project: &str, name: &str| {
        if with_project {
            format!("{project}/{name}")
        } else {
            name.to_string()
        }
    };
    let quiet = |working, background_since, last_activity| match attention::activity(
        working,
        background_since,
        last_activity,
        now,
    ) {
        Some(Activity::Background(since)) => format!("background {}", span(since, now)),
        Some(Activity::Quiet(since)) => format!("quiet {}", span(since, now)),
        Some(Activity::Working) | None => String::new(),
    };
    let features = features.iter().map(|f| {
        let label = match f.attention.kind {
            AttentionKind::None if f.progress == Progress::Ready => Progress::Wip.to_string(),
            AttentionKind::None => f.progress.to_string(),
            kind => kind.to_string(),
        };
        (
            f.attention.kind,
            [
                name(&f.project, &f.name),
                label,
                agents(f.session_exists, &f.agents),
                quiet(f.working, f.background_since, f.last_activity),
                detail(&f.attention),
            ],
            &f.agents[..],
        )
    });
    let mains = projects.iter().filter_map(|p| {
        let main = p.main.as_ref()?;
        let kind = main.attention.kind;
        (kind != AttentionKind::None).then(|| {
            (
                kind,
                [
                    name(&p.name, scope::MAIN),
                    kind.to_string(),
                    agents(main.session_exists, &main.agents),
                    quiet(main.working, main.background_since, main.last_activity),
                    detail(&main.attention),
                ],
                &main.agents[..],
            )
        })
    });
    let mut rendered: Vec<(AttentionKind, [String; 5], &[AgentSnapshot])> =
        features.chain(mains).collect();
    rendered.sort_by_key(|(kind, _, _)| *kind);
    let widths: Vec<usize> = (0..5)
        .map(|col| {
            rendered
                .iter()
                .map(|(_, r, _)| r[col].len())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rendered
        .iter()
        .flat_map(|(_, cells, agents)| {
            // A column empty in every row takes no space.
            let row = cells
                .iter()
                .zip(&widths)
                .filter(|(_, width)| **width > 0)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_string();
            std::iter::once(row).chain(latest_lines(agents, "    "))
        })
        .collect()
}

/// `<indent><agent>: <latest line>` for each of `agents` that has one.
fn latest_lines<'a>(
    agents: &'a [AgentSnapshot],
    indent: &'a str,
) -> impl Iterator<Item = String> + 'a {
    agents
        .iter()
        .filter_map(move |a| Some(latest_line(indent, &a.name, a.latest.as_ref()?)))
}

fn latest_line(indent: &str, agent: &str, latest: &Latest) -> String {
    let width = attention::LATEST_WIDTH.saturating_sub(agent.chars().count() + 2);
    format!("{indent}{agent}: {}", attention::cut(&latest.text, width))
}

/// `agent:state` for each agent, `+N` for its unread messages; `no session`
/// when the scope's session is closed.
fn agents(session_exists: bool, agents: &[AgentSnapshot]) -> String {
    if !session_exists {
        return "no session".to_string();
    }
    agents
        .iter()
        .map(|a| match a.unread {
            0 => format!("{}:{}", a.name, a.state),
            n => format!("{}:{}+{n}", a.name, a.state),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a stalled scope is doing, which its attention has no detail for.
pub(crate) const STALLED: &str = "every agent idle, no unread messages";

fn detail(attention: &Attention) -> String {
    match (attention.kind, &attention.agent, &attention.detail) {
        (AttentionKind::Blocked, Some(agent), Some(reason)) => format!("{agent}: {reason}"),
        (AttentionKind::Blocked, Some(agent), None) => format!("{agent}: (no reason given)"),
        (AttentionKind::Stalled, _, _) => STALLED.to_string(),
        (_, _, Some(detail)) => detail.clone(),
        _ => String::new(),
    }
}

/// The summary's first line of text, skipping headings.
pub(crate) fn first_line(project_root: &Path, name: &str) -> Option<String> {
    let summary = std::fs::read_to_string(paths::summary_path(project_root, name)).ok()?;
    summary
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
}

fn age(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    if (now - then).num_seconds() < 60 {
        return "just now".to_string();
    }
    format!("{} ago", span(then, now))
}

/// How long ago `then` was, rounded down to its largest unit: `5m`, `3h`,
/// `2d`.
pub(crate) fn span(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        0..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::attention::AgentState;
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
            crate::testing::NO_SERVER,
        )
        .unwrap();
        write_summary(&project, "auth", "# auth\n\nAdds OAuth login\n\n## More\n");
        feat_status(
            &project,
            "auth",
            Progress::Ready,
            None,
            None,
            crate::testing::NO_SERVER,
        )
        .unwrap();

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
            crate::testing::NO_SERVER,
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
    fn feature_view_lists_each_agents_latest_line_before_the_summary() {
        use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        let agents_dir = paths::agents_dir(&project);
        let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        registry.register(
            "worker",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: "s1".into(),
                window_name: "worker".into(),
                active: true,
                agent_definition: None,
                harness: crate::harness::Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, "login").unwrap();
        let transcript = dir.path().join("s1.jsonl");
        let reply = serde_json::json!({"type": "assistant", "uuid": "a1",
            "message": {"content": [{"type": "text", "text": "Login works"}]}});
        std::fs::write(&transcript, format!("{reply}\n")).unwrap();
        runtime::write_session_path(
            &project,
            "login",
            "worker",
            runtime::SessionPath::Transcript,
            Some(&transcript),
        )
        .unwrap();

        let lines = feature(&project, "login").unwrap();
        assert_eq!(
            lines[1..4],
            ["latest:", "  worker: Login works", "no summary"]
        );
    }

    fn scope_agent(name: &str, state: AgentState) -> AgentSnapshot {
        AgentSnapshot {
            name: name.into(),
            state,
            unread: 0,
            window: None,
            pane: None,
            waiting: None,
            latest: None,
            denied: None,
        }
    }

    fn snapshot_feature(
        name: &str,
        attention: Attention,
        agents: Vec<AgentSnapshot>,
        last_activity: Option<DateTime<Utc>>,
    ) -> FeatureSnapshot {
        FeatureSnapshot {
            project: "app".into(),
            name: name.into(),
            attention,
            progress: Progress::Wip,
            blocked_reason: None,
            blocked_by: None,
            summary: None,
            lifecycle: crate::state::feature::FeatureStatus::Wip,
            pr: None,
            session: format!("app/{name}"),
            session_exists: true,
            agents,
            working: false,
            background_since: None,
            last_activity,
        }
    }

    fn needs(kind: AttentionKind, detail: Option<&str>) -> Attention {
        Attention {
            kind,
            detail: detail.map(str::to_string),
            agent: None,
        }
    }

    #[test]
    fn a_main_needing_attention_is_ranked_among_the_features_and_quiet_shows_how_long() {
        let hours = |h| Some(Utc::now() - chrono::Duration::hours(h));
        let features = [
            snapshot_feature(
                "login",
                needs(AttentionKind::Dead, Some("qa: window missing")),
                vec![scope_agent("qa", AgentState::Dead)],
                None,
            ),
            snapshot_feature(
                "search",
                needs(AttentionKind::Stalled, None),
                vec![scope_agent("implementer", AgentState::Idle)],
                hours(3),
            ),
        ];
        let main = |attention: Attention| ProjectSnapshot {
            name: "app".into(),
            root: "/src/app".into(),
            skipped: None,
            home: false,
            main: Some(attention::ScopeSnapshot {
                session: "app/main".into(),
                session_exists: true,
                agents: vec![scope_agent("main", AgentState::Asking)],
                attention,
                progress: Progress::Wip,
                blocked_reason: None,
                blocked_by: None,
                working: false,
                background_since: None,
                last_activity: None,
            }),
        };

        let lines = rows(
            &features,
            &[main(needs(
                AttentionKind::Asking,
                Some("main: plan approval"),
            ))],
            true,
        );
        assert_eq!(
            lines,
            vec![
                "app/main    asking   main:asking                 main: plan approval",
                "app/login   dead     qa:dead                     qa: window missing",
                "app/search  stalled  implementer:idle  quiet 3h  \
                 every agent idle, no unread messages",
            ]
        );

        let lines = rows(&features, &[main(needs(AttentionKind::None, None))], false);
        assert_eq!(lines.len(), 2, "a main that needs nothing has no row");
        assert!(lines[0].starts_with("login "), "{lines:?}");
    }

    #[test]
    fn a_ready_feature_reads_wip_while_an_agent_is_busy() {
        let row = |state| {
            let mut f = snapshot_feature(
                "login",
                needs(AttentionKind::None, None),
                vec![scope_agent("implementer", state)],
                None,
            );
            f.progress = Progress::Ready;
            f.attention = attention::attention(&f);
            rows(&[f], &[], false).remove(0)
        };
        assert_eq!(row(AgentState::Busy), "login  wip  implementer:busy");
        assert_eq!(row(AgentState::Idle), "login  ready  implementer:idle");
    }

    #[test]
    fn each_agents_latest_line_follows_its_scopes_row_cut_to_fit() {
        let said = |name: &str, text: &str| AgentSnapshot {
            latest: Some(Latest {
                text: text.into(),
                at: None,
            }),
            ..scope_agent(name, AgentState::Idle)
        };
        let features = [
            snapshot_feature(
                "login",
                needs(AttentionKind::None, None),
                vec![
                    said("implementer", "Fixed the bug"),
                    scope_agent("qa", AgentState::Idle),
                    said("reviewer", &"word ".repeat(40)),
                ],
                None,
            ),
            snapshot_feature("api", needs(AttentionKind::None, None), Vec::new(), None),
        ];
        let lines = rows(&features, &[], false);
        assert_eq!(
            lines[0],
            "login  wip  implementer:idle qa:idle reviewer:idle"
        );
        assert_eq!(lines[1], "    implementer: Fixed the bug");
        assert!(
            lines[2].starts_with("    reviewer: word word"),
            "{}",
            lines[2]
        );
        assert!(lines[2].ends_with("word…"), "{}", lines[2]);
        assert!(lines[2].trim_start().chars().count() <= attention::LATEST_WIDTH);
        assert_eq!(lines[3], "api    wip");
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn feature_view_dates_the_last_activity_by_its_agents() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        runtime::set_activity(
            &project,
            "login",
            "implementer",
            Utc::now() - chrono::Duration::hours(3),
        );

        let lines = feature(&project, "login").unwrap();

        assert_eq!(lines[0], "login  wip  (last active 3h ago)");
    }

    #[test]
    fn a_blocked_main_has_a_view_of_its_own_and_a_row_in_the_projects() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        assert_eq!(feature(&project, "main").unwrap(), ["main  wip"]);

        feat_status(
            &project,
            "main",
            Progress::Blocked,
            Some("next item?"),
            Some("main"),
            crate::testing::NO_SERVER,
        )
        .unwrap();

        assert_eq!(
            feature(&project, "main").unwrap(),
            ["main  blocked", "blocked on: next item? (main)"]
        );
        assert_eq!(
            super::project(&project, None, false, server.name()).unwrap(),
            [
                "main   blocked  no session  main: next item?",
                "login  wip      no session",
            ]
        );
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
