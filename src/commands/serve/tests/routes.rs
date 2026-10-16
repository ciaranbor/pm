use super::*;

#[test]
fn a_request_without_a_live_token_is_refused() {
    let f = fixture();
    let reader = pair(&f.config, "reader");
    let revoked = pair(&f.config, "gone");
    crate::commands::serve_revoke::revoke(&f.config.files, "gone").unwrap();

    let status = |path: &str, token: Option<&str>| get(&f.config, path, token).0;
    assert_eq!(status("/v1/snapshot", None), 401);
    assert_eq!(status("/v1/nowhere", None), 401, "no token learns no paths");
    assert_eq!(status("/v1/snapshot", Some("not-a-token")), 401);
    assert_eq!(status("/v1/snapshot", Some(&revoked)), 401);
    assert_eq!(status("/v1/snapshot", Some(&reader)), 200);

    let bearer = format!("Bearer {reader}");
    let handled = route(
        &f.config,
        "",
        &request("GET", "/v1/snapshot", "", Some(&bearer), ""),
    );
    assert_eq!(handled.device.as_deref(), Some("reader"));
    let lowercase_bearer = format!("bearer {reader}");
    let lowercase = route(
        &f.config,
        "",
        &request("GET", "/v1/snapshot", "", Some(&lowercase_bearer), ""),
    );
    assert_eq!(
        lowercase.reply.status(),
        200,
        "the scheme is case-insensitive"
    );
}

#[test]
fn a_retired_path_is_gone_not_missing_so_an_older_app_blames_itself() {
    let f = fixture();
    let token = pair(&f.config, "phone");

    assert_eq!(get(&f.config, "/v1/device", None).0, 401);
    let (status, body) = get(&f.config, "/v1/device", Some(&token));
    assert_eq!(status, 410, "{body}");
    assert_eq!(get(&f.config, "/v1/nowhere", Some(&token)).0, 404);
}

#[test]
fn the_endpoints_serve_the_snapshot_a_features_details_and_summary_and_an_agents_screen() {
    let f = fixture();
    let token = pair(&f.config, "reader");
    let get = |path: &str| get(&f.config, path, Some(&token));
    let session = crate::tmux::session_name(&f.project_name, "login");
    let window = f
        .server
        .spawn_idle_fake_agent(&f.project, &session, "login", "implementer");
    f.server.wait_for_pane_text(&window, "sleep 999");
    let summary = paths::summary_path(&f.project, "login");
    std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
    std::fs::write(&summary, "Adds login\n\nDetails.\n").unwrap();
    let p = &f.project_name;

    let (status, body) = get("/v1/snapshot");
    assert_eq!(status, 200);
    let snapshot: serde_json::Value = serde_json::from_str(&body).unwrap();
    let expected =
        serde_json::to_value(attention::all(&f.config.projects_dir, f.server.name()).unwrap())
            .unwrap();
    assert_eq!(snapshot["version"], 1);
    assert_eq!(snapshot["features"], expected["features"]);

    assert_eq!(
        get(&format!("/v1/features/{p}/login/summary")),
        (200, "Adds login\n\nDetails.\n".into())
    );
    let features = paths::features_dir(&f.project);
    let mut state = FeatureState::load(&features, "login").unwrap();
    state.context = "Add a login page".into();
    state.workflow = Some("implement-and-review".into());
    state.save(&features, "login").unwrap();
    let (status, body) = get(&format!("/v1/features/{p}/login"));
    assert_eq!(status, 200);
    let info: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(info["context"], "Add a login page");
    assert_eq!(info["branch"], "login");
    assert_eq!(info["base"], "main");
    assert_eq!(info["has_summary"], true);
    assert_eq!(info["pr"], serde_json::Value::Null);
    assert_eq!(info["divergence"]["ahead"], 0);
    assert_eq!(info["divergence"]["behind"], 0);
    assert_eq!(info["workflow"]["name"], "implement-and-review");
    assert!(info["workflow"]["description"].is_string(), "{info}");
    assert_eq!(info["rebase_in_progress"], false);
    for stamp in ["created", "last_active"] {
        chrono::DateTime::parse_from_rfc3339(info[stamp].as_str().unwrap()).unwrap();
    }

    let (status, screen) = get(&format!("/v1/agents/{p}/login/implementer/screen"));
    assert_eq!(status, 200);
    assert!(screen.contains("sleep 999"), "{screen}");

    for missing in [
        "/v1/features/nope/login/summary".to_string(),
        format!("/v1/features/{p}/search/summary"),
        format!("/v1/features/{p}/%2E%2E/summary"),
        format!("/v1/features/{p}/..%2F..%2Fconfig/summary"),
        "/v1/features/nope/login".to_string(),
        format!("/v1/features/{p}/search"),
        format!("/v1/features/{p}/%2E%2E"),
        format!("/v1/features/{p}/..%2F..%2Fconfig"),
        format!("/v1/agents/{p}/login/reviewer/screen"),
        format!("/v1/agents/{p}/%2E%2E/implementer/screen"),
        format!("/v1/agents/{p}/login/implementer/dialog"),
        format!("/v1/agents/{p}/login/reviewer/dialog"),
        format!("/v1/agents/{p}/%2E%2E/implementer/dialog"),
        "/v1/nowhere".to_string(),
    ] {
        assert_eq!(get(&missing).0, 404, "{missing}");
    }
}
