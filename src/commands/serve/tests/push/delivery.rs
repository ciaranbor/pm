use super::*;

#[test]
fn a_transition_and_its_end_are_pushed_encrypted_to_each_subscriber_until_its_service_drops_it() {
    let mut f = fixture();
    f.config.push = PushPolicy::local();
    let phone = pair(&f.config, "phone");
    pair(&f.config, "unsubscribed");
    let (url, received) = push_service(vec![201, 410]);
    let (secret, auth, subscription) = subscriber(&url);
    let push = serve_push::subscription(&subscription, &PushPolicy::local()).unwrap();
    Devices::update(&f.config.files, |d| {
        d.devices.get_mut("phone").unwrap().push = Some(crate::state::devices::Push {
            endpoint: url.clone(),
            ..push.clone()
        });
        Ok(())
    })
    .unwrap();
    let server = start(f.config.clone());
    let vapid: serde_json::Value = serde_json::from_str(
        &ureq::get(format!("http://{}/v1/push", server.0.addr()))
            .header("Authorization", format!("Bearer {phone}"))
            .call()
            .unwrap()
            .body_mut()
            .read_to_string()
            .unwrap(),
    )
    .unwrap();

    feat_status(
        &f.project,
        "login",
        Progress::Blocked,
        Some("which DB?"),
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
    let pushed = received.recv_timeout(Duration::from_secs(15)).unwrap();
    assert_eq!(pushed.header("content-encoding"), Some("aes128gcm"));
    let authorization = pushed.header("authorization").unwrap();
    assert!(authorization.starts_with("vapid t="), "{authorization}");
    assert!(
        authorization.ends_with(&format!("k={}", vapid["vapid"].as_str().unwrap())),
        "{authorization}"
    );
    assert_eq!(pushed.header("urgency"), Some("high"));
    let auth = web_push_native::Auth::clone_from_slice(&auth);
    let decrypt = |body: Vec<u8>| -> serde_json::Value {
        serde_json::from_slice(&web_push_native::decrypt(body, &secret, &auth).unwrap()).unwrap()
    };
    let message = decrypt(pushed.body);
    assert_eq!(
        message,
        serde_json::json!({
            "project": f.project_name,
            "scope": "login",
            "kind": "blocked",
            "agent": null,
        })
    );

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
    let pushed = received.recv_timeout(Duration::from_secs(15)).unwrap();
    assert_eq!(pushed.header("urgency"), Some("normal"));
    assert_eq!(
        decrypt(pushed.body),
        serde_json::json!({
            "project": f.project_name,
            "scope": "login",
            "ended": "blocked",
            "agent": null,
        }),
        "the end goes before what began with it"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while stored_push(&f.config, "phone").is_some() {
        assert!(Instant::now() < deadline, "a 410 drops the subscription");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        received.try_recv().is_err(),
        "nothing more once the subscription is gone"
    );
    drop(server);
}

#[test]
fn a_push_service_redirecting_is_not_followed() {
    let mut f = fixture();
    f.config.push = PushPolicy::local();
    pair(&f.config, "phone");
    let (elsewhere, followed) = push_service(vec![201]);
    let (url, received) = push_service_answering(vec![
        format!("HTTP/1.1 303 X\r\nLocation: {elsewhere}\r\nContent-Length: 0\r\n\r\n"),
        "HTTP/1.1 201 X\r\nContent-Length: 0\r\n\r\n".into(),
    ]);
    let (_, _, subscription) = subscriber(&url);
    let push = serve_push::subscription(&subscription, &PushPolicy::local()).unwrap();
    Devices::update(&f.config.files, |d| {
        d.devices.get_mut("phone").unwrap().push = Some(push);
        Ok(())
    })
    .unwrap();
    let _server = start(f.config.clone());

    feat_status(
        &f.project,
        "login",
        Progress::Blocked,
        Some("which DB?"),
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
    received.recv_timeout(Duration::from_secs(15)).unwrap();
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
    received.recv_timeout(Duration::from_secs(15)).unwrap();
    assert!(
        followed.try_recv().is_err(),
        "the redirect's target is never sent to"
    );
}

#[test]
fn a_transition_is_pushed_once_its_grace_is_out_and_not_at_all_if_its_need_ends_first() {
    let mut f = fixture();
    f.config.push = PushPolicy::local();
    f.config.push_grace = Duration::from_millis(300);
    let token = pair(&f.config, "phone");
    let (url, received) = push_service(vec![201, 201]);
    subscribe(&f.config, "phone", &url);
    let server = start(f.config.clone());
    let events = watched(&server, &token);

    block(&f);
    assert_eq!(next_transition(&events), "blocked");
    received
        .recv_timeout(Duration::from_secs(15))
        .expect("pushed with nothing else happening");

    feat_status(
        &f.project,
        "login",
        Progress::Wip,
        None,
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
    received.recv_timeout(Duration::from_secs(15)).unwrap();
    drop(server);

    f.config.push_grace = Duration::from_secs(3);
    let (url, received) = push_service(vec![201]);
    subscribe(&f.config, "phone", &url);
    let server = start(f.config.clone());
    let events = watched(&server, &token);
    block(&f);
    assert_eq!(next_transition(&events), "blocked");
    feat_status(
        &f.project,
        "login",
        Progress::Wip,
        None,
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
    assert!(
        received.recv_timeout(Duration::from_secs(5)).is_err(),
        "neither the transition nor its end is pushed"
    );
    drop(server);
}

#[test]
fn a_flush_sends_the_transitions_held_back() {
    use crate::commands::attention::{Attention, AttentionKind, transition::Transition};
    let mut f = fixture();
    f.config.push = PushPolicy::local();
    pair(&f.config, "phone");
    let (url, received) = push_service(vec![201]);
    subscribe(&f.config, "phone", &url);
    let key = serve_push::vapid_key(&f.config.files.key()).unwrap();
    let pusher = serve_push::Pusher::start(
        f.config.files.clone(),
        key,
        PushPolicy::local(),
        Duration::from_secs(600),
    );

    pusher.send(
        vec![Transition {
            project: f.project_name.clone(),
            scope: "login".into(),
            attention: Attention {
                kind: AttentionKind::Blocked,
                detail: None,
                agent: None,
            },
        }],
        Vec::new(),
    );
    pusher.flush();
    received
        .recv_timeout(Duration::from_secs(5))
        .expect("sent long before its grace is out");
}
