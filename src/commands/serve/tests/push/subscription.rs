use super::*;

#[test]
fn a_device_sets_and_clears_only_its_own_https_subscription() {
    let f = fixture();
    let phone = pair(&f.config, "phone");
    pair(&f.config, "other");

    let (status, body) = call(&f.config, "GET", "/v1/push", &phone, "");
    assert_eq!(
        (status, body.as_str()),
        (200, r#"{"vapid":"the-servers-vapid-key"}"#)
    );
    assert!(
        !f.config.files.key().exists(),
        "the route never makes a key of its own"
    );

    let (_, _, subscription) = subscriber("https://ntfy.sh/upAbc?up=1");
    assert_eq!(
        call(&f.config, "PUT", "/v1/push", &phone, &subscription).0,
        204
    );
    let stored = stored_push(&f.config, "phone").unwrap();
    assert_eq!(stored.endpoint, "https://ntfy.sh/upAbc?up=1");
    assert_eq!(stored_push(&f.config, "other"), None);

    let (_, _, plain) = subscriber("http://ntfy.sh/upAbc");
    let (_, _, unknown) = subscriber("https://tailnet-service.ts.net/up");
    let bad_key = subscription.replace("\"p256dh\":\"", "\"p256dh\":\"AA");
    for refused in [
        plain.as_str(),
        unknown.as_str(),
        bad_key.as_str(),
        "{}",
        "not json",
    ] {
        assert_eq!(
            call(&f.config, "PUT", "/v1/push", &phone, refused).0,
            400,
            "{refused}"
        );
    }
    assert_eq!(stored_push(&f.config, "phone"), Some(stored));

    assert_eq!(call(&f.config, "DELETE", "/v1/push", &phone, "").0, 204);
    assert_eq!(stored_push(&f.config, "phone"), None);
    assert_eq!(call(&f.config, "POST", "/v1/snapshot", &phone, "").0, 405);
}

#[test]
fn a_stored_subscription_the_policy_refuses_is_dropped() {
    let f = fixture();
    pair(&f.config, "phone");
    let (_, _, subscription) = subscriber("https://tailnet-service.ts.net/up");
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
    let deadline = Instant::now() + Duration::from_secs(15);
    while stored_push(&f.config, "phone").is_some() {
        assert!(
            Instant::now() < deadline,
            "a refused subscription is dropped"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
