use chrono::Utc;

use super::*;
use crate::commands::running_agents::STARTING_SECS;
use crate::commands::{feat_new, feat_status::feat_status, init};
use crate::harness::Harness;
use crate::messages;
use crate::state::feature::{FeatureStatus, Progress};
use crate::state::paths;
use crate::state::runtime::{self, Waiting, WaitingKind};
use crate::testing::TestServer;
use crate::tmux;
use tempfile::tempdir;

#[test]
fn a_marker_refines_only_an_agent_its_window_reads_as_busy() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, _, project_name) = server.setup_project_with_feature(dir.path(), "login");
    let session = tmux::session_name(&project_name, "login");
    server.spawn_fake_agent(&project, &session, "login", "asking");
    server.spawn_fake_agent(&project, &session, "login", "starting");
    server.spawn_idle_fake_agent(&project, &session, "login", "idle");
    server.spawn_dead_fake_agent(&project, &session, "login", "dead");
    server.spawn_dead_fake_agent(&project, &session, "login", "spawned");
    server.spawn_dead_fake_agent(&project, &session, "login", "never-started");
    server.spawn_dead_fake_agent(&project, &session, "login", "in-rc");
    server.spawn_dead_fake_agent(&project, &session, "login", "stuck-in-rc");
    let mark = |agent: &str, kind: WaitingKind| {
        let waiting = runtime::Waiting::now(kind, Some("Which DB?".into()));
        runtime::write_waiting(&project, "login", agent, &waiting).unwrap();
    };
    mark("asking", WaitingKind::Question);
    mark("starting", WaitingKind::Startup);
    mark("dead", WaitingKind::Question);
    mark("spawned", WaitingKind::Startup);
    let spawned_ago = |agent: &str, secs: i64| {
        let mut stale = runtime::Waiting::now(WaitingKind::Startup, None);
        stale.since -= chrono::Duration::seconds(secs);
        runtime::write_waiting(&project, "login", agent, &stale).unwrap();
    };
    let past_starting = STARTING_SECS + 1;
    spawned_ago("never-started", past_starting);
    let launched = runtime::reset_launched(&project, "login", "never-started").unwrap();
    std::fs::File::create(&launched)
        .unwrap()
        .set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(past_starting as u64),
        )
        .unwrap();
    spawned_ago("in-rc", past_starting);
    let past_launch = crate::commands::launch_check::START_WITHIN.as_secs() as i64 + 1;
    spawned_ago("stuck-in-rc", past_launch);
    runtime::touch_activity(&project, "login", "asking").unwrap();

    let login = &super::project(&project, server.name()).unwrap().features[0];

    let states: Vec<(&str, AgentState, Option<&str>)> = login
        .agents
        .iter()
        .map(|a| {
            let detail = a.waiting.as_ref().map(|w| w.detail.as_str());
            (a.name.as_str(), a.state, detail)
        })
        .collect();
    assert_eq!(
        states,
        [
            ("asking", AgentState::Asking, Some("Which DB?")),
            ("dead", AgentState::Dead, None),
            ("idle", AgentState::Idle, None),
            ("in-rc", AgentState::Busy, None),
            ("never-started", AgentState::Dead, None),
            ("spawned", AgentState::Busy, None),
            ("starting", AgentState::Busy, None),
            ("stuck-in-rc", AgentState::Dead, None),
        ]
    );
    assert_eq!(login.attention.kind, AttentionKind::Asking);
    assert!(!login.working, "its only busy agent was never active");
    assert_eq!(login.agents[0].waiting.as_ref().unwrap().dialog, None);

    // A dialog whose hook (this process) waits names it.
    let payload = serde_json::json!({
        "hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion",
        "tool_input": {"questions": [{"question": "Which DB?", "options": []}]}
    });
    let (dialog, reply_context) = Harness::ClaudeCode.dialog(&payload).unwrap();
    let id = dialog.id.clone();
    let record = runtime::DialogRecord {
        dialog,
        pid: Some(std::process::id()),
        reply_context,
    };
    runtime::write_dialog(&project, "login", "asking", &record).unwrap();
    let login = &super::project(&project, server.name()).unwrap().features[0];
    assert_eq!(
        login.agents[0].waiting.as_ref().unwrap().dialog,
        Some(id.clone())
    );

    // Answered from the app: busy before its hook re-marks the agent,
    // and asking again at a dialog that opens after.
    runtime::close_dialog(&project, "login", "asking", &id, true).unwrap();
    let login = &super::project(&project, server.name()).unwrap().features[0];
    assert_eq!(login.agents[0].state, AgentState::Busy);
    let next = Waiting::now(WaitingKind::Permission, Some("Bash: ls".into()));
    runtime::write_waiting(&project, "login", "asking", &next).unwrap();
    let login = &super::project(&project, server.name()).unwrap().features[0];
    assert_eq!(login.agents[0].state, AgentState::Asking);

    let at = |minutes| Utc::now() - chrono::Duration::minutes(minutes);
    runtime::set_activity(&project, "login", "starting", at(1));
    let login = &super::project(&project, server.name()).unwrap().features[0];
    assert!(login.working);

    runtime::set_activity(&project, "login", "starting", at(30));
    runtime::set_activity(&project, "login", "asking", at(25));
    let login = &super::project(&project, server.name()).unwrap().features[0];
    assert!(!login.working, "a busy agent silent for 30 minutes");
    let quiet = Utc::now() - login.last_activity.unwrap();
    assert_eq!(quiet.num_minutes(), 25, "the latest of its agents");
}

#[test]
fn a_stopped_loop_shows_its_reason_over_any_marker_but_a_dialog() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, _, project_name) = server.setup_project_with_feature(dir.path(), "login");
    let session = tmux::session_name(&project_name, "login");
    server.spawn_fake_agent(&project, &session, "login", "qa");
    runtime::trip_loop(&project, "login", "qa", "inbox unreadable").unwrap();
    let shown = |kind: Option<WaitingKind>| {
        match kind {
            Some(kind) => {
                let marker = Waiting::now(kind, Some("Which DB?".into()));
                runtime::write_waiting(&project, "login", "qa", &marker).unwrap();
            }
            None => {
                runtime::clear_waiting(&project, "login", "qa").unwrap();
            }
        }
        let login = &super::project(&project, server.name()).unwrap().features[0];
        let qa = &login.agents[0];
        (qa.state, qa.waiting.as_ref().unwrap().detail.clone())
    };
    let stopped = (
        AgentState::Unarmed,
        "loop stopped: inbox unreadable".to_string(),
    );

    assert_eq!(shown(None), stopped);
    assert_eq!(shown(Some(WaitingKind::Prompt)), stopped);
    assert_eq!(shown(Some(WaitingKind::Background)), stopped);
    assert_eq!(
        shown(Some(WaitingKind::Question)),
        (AgentState::Asking, "Which DB?".to_string())
    );
}

#[test]
fn background_work_is_not_working_and_dates_from_the_oldest_wait() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, _, project_name) = server.setup_project_with_feature(dir.path(), "login");
    let session = tmux::session_name(&project_name, "login");
    let started = |agent: &str, hours| {
        let target = server.spawn_fake_agent(&project, &session, "login", agent);
        // The harness itself stands for its background work's waiter.
        let processes = tmux::pane_processes(server.name(), &target).unwrap();
        let waiter = processes.last().unwrap().pid;
        runtime::take_waiter(&project, "login", agent, waiter, None).unwrap();
        let mut waiting = runtime::Waiting::now(WaitingKind::Background, None);
        waiting.since -= chrono::Duration::hours(hours);
        runtime::write_waiting(&project, "login", agent, &waiting).unwrap();
        runtime::touch_activity(&project, "login", agent).unwrap();
        waiting.since
    };
    let oldest = started("build", 30);
    started("eval", 2);

    let login = &super::project(&project, server.name()).unwrap().features[0];
    assert!(!login.working);
    assert_eq!(login.background_since, Some(oldest));
    assert_eq!(login.attention.kind, AttentionKind::None, "not stalled");

    server.spawn_fake_agent(&project, &session, "login", "implementer");
    runtime::touch_activity(&project, "login", "implementer").unwrap();
    let login = &super::project(&project, server.name()).unwrap().features[0];
    assert!(login.working, "a busy agent is");
}

#[test]
fn a_team_with_no_running_agent_is_not_stalled() {
    let mut f = feature(Progress::Wip, FeatureStatus::Wip);
    assert_eq!(kind(&f), AttentionKind::None);
    f.agents = vec![agent("implementer", AgentState::Closed, 0)];
    assert_eq!(kind(&f), AttentionKind::None);
}

#[test]
fn a_dead_agent_says_whether_its_window_or_its_harness_is_gone() {
    let mut f = feature(Progress::Wip, FeatureStatus::Wip);
    f.agents = vec![
        agent("implementer", AgentState::Busy, 0),
        agent("reviewer", AgentState::Dead, 0),
    ];
    assert_eq!(
        attention(&f),
        Attention {
            kind: AttentionKind::Dead,
            detail: Some("reviewer: window missing".into()),
            agent: Some("reviewer".into()),
        }
    );
    f.agents[1].window = Some("app/login:2".into());
    assert_eq!(
        attention(&f).detail.as_deref(),
        Some("reviewer: harness exited")
    );
}

#[test]
fn the_project_snapshot_reads_each_agents_window() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, _, project_name) = server.setup_project_with_feature(dir.path(), "login");
    let session = tmux::session_name(&project_name, "login");
    server.spawn_idle_fake_agent(&project, &session, "login", "implementer");
    server.spawn_dead_fake_agent(&project, &session, "login", "reviewer");
    server.spawn_fake_agent(&project, &session, "login", "qa");
    messages::send(&paths::messages_dir(&project), "login", "qa", "user", "hi").unwrap();

    let snapshot = super::project(&project, server.name()).unwrap();

    let login = &snapshot.features[0];
    assert!(login.session_exists);
    let states: Vec<(&str, AgentState, u32)> = login
        .agents
        .iter()
        .map(|a| (a.name.as_str(), a.state, a.unread))
        .collect();
    assert_eq!(
        states,
        [
            ("implementer", AgentState::Idle, 0),
            ("qa", AgentState::Busy, 1),
            ("reviewer", AgentState::Dead, 0),
        ]
    );
    assert_eq!(login.attention.kind, AttentionKind::Dead);
    assert_eq!(
        login.attention.detail.as_deref(),
        Some("reviewer: harness exited")
    );
}

#[test]
fn an_agent_without_a_window_is_dead_only_while_its_session_is_open() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, _, project_name) = server.setup_project_with_feature(dir.path(), "login");
    let session = tmux::session_name(&project_name, "login");
    let window = server.spawn_idle_fake_agent(&project, &session, "login", "implementer");
    tmux::kill_window(server.name(), &window).unwrap();

    let login = &super::project(&project, server.name()).unwrap().features[0];
    assert_eq!(login.agents[0].state, AgentState::Dead);
    assert_eq!(login.agents[0].window, None);
    assert_eq!(login.attention.kind, AttentionKind::Dead);

    tmux::kill_session(server.name(), &session).unwrap();
    let login = &super::project(&project, server.name()).unwrap().features[0];
    assert!(!login.session_exists);
    assert_eq!(login.agents[0].state, AgentState::Closed);
    assert_eq!(login.attention.kind, AttentionKind::None);
}

#[test]
fn every_project_here_is_snapshotted_most_urgent_first_and_an_unreadable_one_skipped() {
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
    feat_status(
        &roots[1],
        "search",
        Progress::Blocked,
        Some("why?"),
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
    let gone = dir.path().join(server.scope("gone"));
    init::init(&gone, &projects_dir, None, server.name()).unwrap();
    std::fs::remove_dir_all(&gone).unwrap();
    // Its checkout is here, but not its pm state.
    let cleared = dir.path().join(server.scope("cleared"));
    init::init(&cleared, &projects_dir, None, server.name()).unwrap();
    std::fs::remove_dir_all(paths::pm_dir(&cleared)).unwrap();
    let broken = projects_dir.join(format!("{}.toml", server.scope("broken")));
    std::fs::write(&broken, "root = \"/x\"\nmain_branch = [").unwrap();

    let snapshot = all(&projects_dir, server.name()).unwrap();

    let features: Vec<(String, &str, AttentionKind)> = snapshot
        .features
        .iter()
        .map(|f| (f.project.clone(), f.name.as_str(), f.attention.kind))
        .collect();
    assert_eq!(
        features,
        [
            (server.scope("beta"), "search", AttentionKind::Blocked),
            (server.scope("alpha"), "login", AttentionKind::None),
        ]
    );
    let skipped: Vec<(&str, Option<&str>)> = snapshot
        .projects
        .iter()
        .map(|p| (p.name.as_str(), p.skipped.as_deref()))
        .collect();
    assert_eq!(
        skipped.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        [
            server.scope("alpha"),
            server.scope("beta"),
            server.scope("broken"),
            server.scope("cleared")
        ]
    );
    assert!(skipped.contains(&(server.scope("alpha").as_str(), None)));
    let no_pm = format!("no pm project at {}", cleared.display());
    assert!(skipped.contains(&(server.scope("cleared").as_str(), Some(no_pm.as_str()))));

    let lines =
        crate::commands::feat_status_view::all(&projects_dir, false, server.name()).unwrap();
    let words: Vec<Vec<&str>> = lines
        .iter()
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert_eq!(
        words[..2],
        [
            vec![
                format!("{}/search", server.scope("beta")).as_str(),
                "blocked",
                "why?"
            ],
            vec![format!("{}/login", server.scope("alpha")).as_str(), "wip"],
        ]
    );
    assert!(
        lines[2].starts_with(&format!(
            "{}: skipped (registry entry {} unreadable: line 2: ",
            server.scope("broken"),
            broken.display()
        )),
        "{}",
        lines[2]
    );
    assert_eq!(
        lines[3],
        format!("{}: skipped ({no_pm})", server.scope("cleared"))
    );
    assert_eq!(lines.len(), 4);
}
