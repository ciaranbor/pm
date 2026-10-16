use super::*;

#[test]
fn an_agents_mode_is_served_with_its_harnesses_modes_and_set_only_to_one_of_them() {
    use crate::harness::Harness;
    use crate::state::agent::{AgentEntry, AgentRegistry};
    let f = fixture();
    let phone = pair(&f.config, "phone");
    let session = crate::tmux::session_name(&f.project_name, "login");
    f.server
        .spawn_prompting_fake_agent(&f.project, &session, "login", "implementer");
    f.server
        .spawn_dead_fake_agent(&f.project, &session, "login", "reviewer");
    let agents_dir = paths::agents_dir(&f.project);
    let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
    registry.register(
        "qa",
        AgentEntry {
            harness: Harness::OpenCode,
            ..agent_entry("qa")
        },
    );
    registry.save(&agents_dir, "login").unwrap();
    let path = |agent: &str| format!("/v1/agents/{}/login/{agent}/mode", f.project_name);
    let post = |agent: &str, body: &str| {
        let (status, body) = call(&f.config, "POST", &path(agent), &phone, body);
        (
            status,
            serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        )
    };

    let (status, body) = get(&f.config, &path("implementer"), Some(&phone));
    assert_eq!(status, 200, "{body}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["mode"], serde_json::Value::Null);
    let ids: Vec<&str> = body["modes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "default",
            "acceptEdits",
            "plan",
            "auto",
            "bypassPermissions"
        ]
    );
    assert_eq!(body["modes"][0]["label"], "Manual");

    assert_eq!(get(&f.config, &path("qa"), Some(&phone)).0, 404);
    assert_eq!(post("qa", r#"{"mode":"plan"}"#).0, 404);
    assert_eq!(post("implementer", r#"{"mode":"dontAsk"}"#).0, 400);
    assert_eq!(post("implementer", r#"{"mode":7}"#).0, 400);

    let (status, body) = post("implementer", r#"{"mode":"plan"}"#);
    assert_eq!(status, 409);
    assert_eq!(body["refused"], "mode-unknown");
    let (status, body) = post("reviewer", r#"{"mode":"plan"}"#);
    assert_eq!(status, 409);
    assert_eq!(body["refused"], "not-running");

    let authorization = format!("Bearer {phone}");
    let routed = route(
        &f.config,
        "",
        &request(
            "POST",
            &path("implementer"),
            "",
            Some(&authorization),
            r#"{"mode":"acceptEdits"}"#,
        ),
    );
    assert_eq!(routed.detail.as_deref(), Some("mode acceptEdits"));
}
