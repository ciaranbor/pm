use super::*;

#[test]
fn typed_text_is_confirmed_from_the_conversation_and_keys_are_checked() {
    use crate::state::runtime::{self, Waiting, WaitingKind};
    let f = fixture();
    let typist = pair(&f.config, "typist");
    let session = crate::tmux::session_name(&f.project_name, "login");
    let window = f
        .server
        .spawn_prompting_fake_agent(&f.project, &session, "login", "implementer");
    f.server.wait_for_pane_text(&window, "❯");
    let transcript = register_conversation(&f.project, "implementer", "s1");
    append(&transcript, &typed("u1", "earlier"));
    let mark = |kind| {
        let waiting = Waiting::now(kind, None);
        runtime::write_waiting(&f.project, "login", "implementer", &waiting).unwrap();
    };
    mark(WaitingKind::Prompt);
    let agent = format!("/v1/agents/{}/login/implementer", f.project_name);
    let post = |token: &str, action: &str, body: &str| {
        call(&f.config, "POST", &format!("{agent}/{action}"), token, body)
    };

    let agent_types = std::thread::spawn({
        let server = f.server.name().map(str::to_string);
        let window = window.clone();
        move || {
            for _ in 0..250 {
                let screen = crate::tmux::capture_pane(server.as_deref(), &window).unwrap();
                if screen.contains("deploy it") {
                    append(&transcript, &typed("u2", "deploy it"));
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    });
    let (status, body) = post(&typist, "input", r#"{"text":"deploy it\n"}"#);
    agent_types.join().unwrap();
    assert_eq!(status, 200, "{body}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"delivery": "sent", "confirmed": true})
    );

    assert_eq!(post(&typist, "keys", r#"{"keys":["F1"]}"#).0, 400);
    assert_eq!(post(&typist, "keys", r#"{"keys":["C-1"]}"#).0, 400);
    assert_eq!(post(&typist, "keys", r#"{"keys":["C-Enter"]}"#).0, 400);
    assert_eq!(
        post(&typist, "keys", r#"{"keys":["Down","C-Left","Enter"]}"#).0,
        200
    );
    assert_eq!(post(&typist, "input", r#"{"text":"  "}"#).0, 400);

    mark(WaitingKind::Permission);
    let (status, body) = post(&typist, "input", r#"{"text":"yes"}"#);
    assert_eq!(status, 409);
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["refused"], "asking");
}

#[test]
fn typed_keys_are_checked_and_logged_by_their_hash() {
    let f = fixture();
    let typist = pair(&f.config, "typist");
    let session = crate::tmux::session_name(&f.project_name, "login");
    let window = f
        .server
        .spawn_prompting_fake_agent(&f.project, &session, "login", "implementer");
    f.server.wait_for_pane_text(&window, "❯");
    let path = format!("/v1/agents/{}/login/implementer/type", f.project_name);
    let post = |text: &str| {
        let body = serde_json::json!({ "text": text }).to_string();
        let authorization = format!("Bearer {typist}");
        route(
            &f.config,
            VAPID,
            &request("POST", &path, "", Some(&authorization), &body),
        )
    };
    let status = |text: &str| match post(text).reply {
        Reply::Body { status, .. } => status,
        Reply::Events { .. } => unreachable!(),
    };

    assert_eq!(status(""), 400);
    assert_eq!(status("code\n"), 400);
    assert_eq!(status("a\x1b[A"), 400);
    assert_eq!(status(&"x".repeat(4097)), 400);

    let routed = post("4821-secret");
    assert!(matches!(routed.reply, Reply::Body { status: 200, .. }));
    let detail = routed.detail.unwrap();
    assert!(detail.contains("sha256:"), "{detail}");
    assert!(!detail.contains("secret"), "{detail}");
    f.server.wait_for_pane_text(&window, "❯ 4821-secret");
}
