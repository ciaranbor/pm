use super::*;

#[test]
fn revoking_a_device_ends_its_open_streams_and_no_other_devices() {
    let mut f = fixture();
    f.config.idle_poll = Duration::from_secs(600);
    f.config.watched_poll = Duration::from_secs(600);
    let phone = pair(&f.config, "phone");
    let tablet = pair(&f.config, "tablet");
    let server = start(f.config.clone());
    let revoked = watched(&server, &phone);
    let kept = watched(&server, &tablet);

    crate::commands::serve_revoke::revoke(&f.config.files, "phone").unwrap();

    until(&revoked, |l| l == "event: revoked");
    until(&revoked, |l| l == "0");
    block(&f);
    wake(&f.config.files);
    assert_eq!(next_transition(&kept), "blocked");
}

#[test]
fn a_device_unpairs_itself_and_only_itself() {
    let f = fixture();
    let phone = pair(&f.config, "phone");
    let tablet = pair(&f.config, "tablet");

    assert_eq!(call(&f.config, "GET", "/v1/pairing", &phone, "").0, 405);
    assert_eq!(call(&f.config, "DELETE", "/v1/pairing", &phone, "").0, 204);

    let devices = Devices::load(&f.config.files.devices()).unwrap();
    assert_eq!(devices.devices.keys().collect::<Vec<_>>(), ["tablet"]);
    assert_eq!(call(&f.config, "DELETE", "/v1/pairing", &phone, "").0, 401);
    assert_eq!(get(&f.config, "/v1/snapshot", Some(&tablet)).0, 200);
}
