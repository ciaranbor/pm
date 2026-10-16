use super::*;

#[test]
fn an_event_stream_sends_changes_transitions_and_heartbeats_and_every_reply_its_version() {
    let mut f = fixture();
    f.config.heartbeat = Duration::from_millis(500);
    let token = pair(&f.config, "reader");
    feat_status(
        &f.project,
        "login",
        Progress::Blocked,
        Some("standing"),
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
    let server = start(f.config.clone());

    let version = format!("Pm-Version: {}", crate::version::VERSION);
    let refused = until(&connect(server.0.addr(), "/v1/snapshot", "nope"), |l| {
        l.is_empty()
    });
    assert!(refused[0].contains("401"), "{refused:#?}");
    assert!(
        refused.iter().any(|l| l.eq_ignore_ascii_case(&version)),
        "{refused:#?}"
    );

    let events = connect(server.0.addr(), "/v1/events", &token);
    let opening = until(&events, |l| l.starts_with("data: "));
    assert_eq!(opening[0], "HTTP/1.1 200 OK");
    assert!(opening.contains(&"Content-Type: text/event-stream".to_string()));
    assert!(opening.contains(&version), "{opening:#?}");
    assert!(opening.contains(&"event: snapshot".to_string()));

    let summary = paths::summary_path(&f.project, "login");
    std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
    std::fs::write(&summary, "Adds login\n").unwrap();
    feat_status(
        &f.project,
        "login",
        Progress::Ready,
        None,
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
    let changed = until(&events, |l| l.starts_with("event: transition"));
    assert!(
        changed
            .iter()
            .any(|l| l.starts_with("data: ") && l.contains(r#""progress":"ready""#)),
        "{changed:#?}"
    );
    let transition = until(&events, |l| l.starts_with("data: "));
    let transition: serde_json::Value =
        serde_json::from_str(transition[0].strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(transition["project"], f.project_name.as_str());
    assert_eq!(transition["scope"], "login");
    assert_eq!(transition["kind"], "ready");

    until(&events, |l| l == ": heartbeat");
}

#[test]
fn a_wake_reads_a_change_pm_made_without_waiting_for_the_poll() {
    let mut f = fixture();
    f.config.idle_poll = Duration::from_secs(600);
    f.config.watched_poll = Duration::from_secs(600);
    let token = pair(&f.config, "reader");
    let server = start(f.config.clone());
    let events = watched(&server, &token);

    block(&f);
    wake(&f.config.files);

    assert_eq!(next_transition(&events), "blocked");
}

#[test]
fn an_open_stream_polls_often_for_what_pm_did_not_do() {
    let mut f = fixture();
    f.config.idle_poll = Duration::from_secs(600);
    let token = pair(&f.config, "reader");
    let server = start(f.config.clone());
    let events = watched(&server, &token);

    block(&f);

    assert_eq!(next_transition(&events), "blocked");
}

#[test]
fn wakes_closer_together_than_the_gap_make_one_read() {
    let mut f = fixture();
    f.config.idle_poll = Duration::from_secs(600);
    f.config.watched_poll = Duration::from_secs(600);
    f.config.min_gap = Duration::from_secs(2);
    let token = pair(&f.config, "reader");
    let server = start(f.config.clone());
    let events = watched(&server, &token);

    block(&f);
    wake(&f.config.files);
    // Past the wake's settling, so without the gap it would read `blocked`.
    std::thread::sleep(Duration::from_millis(500));
    let summary = paths::summary_path(&f.project, "login");
    std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
    std::fs::write(&summary, "Adds login\n").unwrap();
    feat_status(
        &f.project,
        "login",
        Progress::Ready,
        None,
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
    wake(&f.config.files);

    assert_eq!(next_transition(&events), "ready", "blocked was never read");
    assert_eq!(server.0.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
}
