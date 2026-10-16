use super::*;

#[test]
fn main_asking_needs_attention_alerts_once_and_unarmed_never_alerts() {
    let asking = main_scope(vec![main_agent(
        AgentState::Asking,
        WaitingKind::Plan,
        "plan approval",
    )]);
    let published = |attention: &str| Options {
        clients: vec![Client::named("c1")],
        sessions: vec![Holder::session(
            "app/main",
            &[(PROJECT, "app"), (ATTENTION, attention)],
        )],
        ..Options::default()
    };
    let now = Utc::now();

    let commands = commands(&asking, &published(""), now);
    assert_eq!(sets(&commands, ATTENTION), ["asking"]);
    assert_eq!(sets(&commands, REASON), ["main: plan approval"]);
    assert_eq!(sets(&commands, BADGE), ["#[fg=red,bold]\u{f059}#[default]"]);
    assert_eq!(sets(&commands, COUNT), ["1"]);
    assert_eq!(
        announced(&commands),
        ["pm: app/main asking: main: plan approval"]
    );
    assert!(announced(&super::commands(&asking, &published("asking"), now)).is_empty());

    let unarmed = main_scope(vec![main_agent(
        AgentState::Unarmed,
        WaitingKind::Interrupted,
        "interrupted",
    )]);
    let commands = super::commands(&unarmed, &published(""), now);
    assert_eq!(sets(&commands, ATTENTION), ["unarmed"]);
    assert_eq!(
        sets(&commands, SUMMARY),
        ["#[fg=magenta]\u{f1f6} 1 unarmed#[default]"]
    );
    assert!(announced(&commands).is_empty());
}

#[test]
fn a_blocked_main_shows_its_attention_badge_and_alerts_once_but_not_when_reopened() {
    let mut snapshot = main_scope(Vec::new());
    snapshot.projects[0].main = Some(ScopeSnapshot::main_of(
        vec![main_agent(AgentState::Idle, WaitingKind::Idle, "idle")],
        TeamStatus {
            progress: Progress::Blocked,
            blocked_reason: Some("next item?".into()),
            blocked_by: Some("main".into()),
        },
    ));
    let session = |options: &[(&'static str, &str)]| Options {
        clients: vec![Client::named("c1")],
        sessions: vec![Holder::session("app/main", options)],
        ..Options::default()
    };
    let now = Utc::now();

    let commands = commands(&snapshot, &session(&[(PROJECT, "app")]), now);
    assert_eq!(sets(&commands, ATTENTION), ["blocked"]);
    assert_eq!(sets(&commands, PROGRESS), ["blocked"]);
    assert_eq!(sets(&commands, REASON), ["next item?"]);
    assert_eq!(
        sets(&commands, BADGE),
        [badge::attention(AttentionKind::Blocked).unwrap()]
    );
    assert_eq!(sets(&commands, COUNT), ["1"]);
    assert_eq!(announced(&commands), ["pm: app/main blocked: next item?"]);

    let shown = session(&[
        (PROJECT, "app"),
        (ATTENTION, "blocked"),
        (ALERTED, "blocked"),
    ]);
    assert!(announced(&super::commands(&snapshot, &shown, now)).is_empty());
    let reopened = super::commands(&snapshot, &session(&[]), now);
    assert!(announced(&reopened).is_empty(), "a standing block");
    assert_eq!(sets(&reopened, ATTENTION), ["blocked"]);
}

#[test]
fn a_blocked_main_shows_a_dialog_and_alerts_it_then_shows_blocked_again_without_alerting() {
    let blocked = TeamStatus {
        progress: Progress::Blocked,
        blocked_reason: Some("next item?".into()),
        blocked_by: Some("main".into()),
    };
    let lead = main_agent(AgentState::Idle, WaitingKind::Idle, "idle");
    let main_with = |helper: AgentState, kind: WaitingKind, detail: &str| {
        let helper = AgentSnapshot {
            name: "helper".into(),
            window: Some("app/main:2".into()),
            ..main_agent(helper, kind, detail)
        };
        let mut snapshot = main_scope(Vec::new());
        snapshot.projects[0].main = Some(ScopeSnapshot::main_of(
            vec![lead.clone(), helper],
            blocked.clone(),
        ));
        snapshot
    };
    let asking = main_with(AgentState::Asking, WaitingKind::Permission, "Bash: ls");
    let answered = main_with(AgentState::Idle, WaitingKind::Idle, "idle");
    let session = |options: &[(&'static str, &str)]| Options {
        clients: vec![Client::named("c1")],
        sessions: vec![Holder::session("app/main", options)],
        ..Options::default()
    };
    let now = Utc::now();
    let alerted_blocked = session(&[
        (PROJECT, "app"),
        (ATTENTION, "blocked"),
        (ALERTED, "blocked"),
    ]);

    let commands = commands(&asking, &alerted_blocked, now);
    assert_eq!(sets(&commands, ATTENTION), ["asking"]);
    assert_eq!(sets(&commands, REASON), ["helper: Bash: ls"]);
    assert_eq!(
        sets(&commands, BADGE),
        [badge::agent(AgentState::Asking, 0)]
    );
    assert_eq!(sets(&commands, ALERTED), ["blocked,asking"]);
    assert_eq!(
        announced(&commands),
        ["pm: app/main asking: helper: Bash: ls"]
    );

    let shown_asking = session(&[
        (PROJECT, "app"),
        (ATTENTION, "asking"),
        (ALERTED, "blocked,asking"),
    ]);
    let commands = super::commands(&answered, &shown_asking, now);
    assert_eq!(sets(&commands, ATTENTION), ["blocked"]);
    assert_eq!(
        sets(&commands, BADGE),
        [badge::attention(AttentionKind::Blocked).unwrap()]
    );
    assert_eq!(sets(&commands, ALERTED), ["blocked"]);
    assert!(announced(&commands).is_empty(), "the same blocked episode");
}

#[test]
fn a_main_session_shows_work_once() {
    let now = Utc::now();
    let main = |lead: AgentState, other: AgentState, working, minutes_ago| {
        let mut snapshot = main_scope(vec![
            AgentSnapshot {
                name: "main".into(),
                state: lead,
                unread: 0,
                window: Some("app/main:1".into()),
                pane: None,
                waiting: None,
                latest: None,
                denied: None,
            },
            AgentSnapshot {
                name: "helper".into(),
                state: other,
                unread: 0,
                window: Some("app/main:2".into()),
                pane: None,
                waiting: None,
                latest: None,
                denied: None,
            },
        ]);
        let scope = snapshot.projects[0].main.as_mut().unwrap();
        scope.working = working;
        scope.last_activity = Some(now - chrono::Duration::minutes(minutes_ago));
        snapshot
    };
    let published = Options {
        sessions: vec![Holder::session("app/main", &[])],
        ..Options::default()
    };
    let activity = |snapshot: &Snapshot| {
        let commands = commands(snapshot, &published, now);
        [ACTIVITY, ACTIVITY_LABEL]
            .iter()
            .flat_map(|name| sets(&commands, name))
            .map(String::from)
            .collect::<Vec<_>>()
    };

    assert!(
        activity(&main(AgentState::Busy, AgentState::Idle, true, 1)).is_empty(),
        "the main agent's badge shows it"
    );
    assert_eq!(
        sets(
            &commands(
                &main(AgentState::Busy, AgentState::Idle, true, 1),
                &published,
                now
            ),
            LABEL
        ),
        ["#[fg=green]\u{f013} busy#[default]"],
        "main's label is its main agent's state"
    );
    assert_eq!(
        activity(&main(AgentState::Idle, AgentState::Busy, true, 1)),
        [
            "#[fg=green]\u{f013}#[default]",
            "#[fg=green]\u{f013} working#[default]"
        ],
        "another agent of main's is working"
    );
    assert_eq!(
        activity(&main(AgentState::Idle, AgentState::Idle, false, 185)),
        [
            "#[fg=colour245]3h#[default]",
            "#[fg=colour245]quiet 3h#[default]"
        ]
    );
    let mut waiting = main(AgentState::Background, AgentState::Idle, false, 185);
    waiting.projects[0].main.as_mut().unwrap().background_since =
        Some(now - chrono::Duration::hours(26));
    assert_eq!(
        activity(&waiting),
        [
            "#[fg=green]\u{f110} 1d#[default]",
            "#[fg=green]\u{f110} background 1d#[default]"
        ],
        "the main agent's badge doesn't show how long"
    );
}
