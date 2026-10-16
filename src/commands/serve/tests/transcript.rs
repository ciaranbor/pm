use super::*;

fn continuation(uuid: &str) -> String {
    line(
        serde_json::json!({"type": "user", "uuid": uuid, "isMeta": true,
        "message": {"role": "user",
                    "content": "Stop hook feedback:\nYou have new messages from reviewer."}}),
    )
}

#[test]
fn an_agents_transcript_is_served_in_pages() {
    let f = fixture();
    let reader = pair(&f.config, "reader");
    let transcript = register_conversation(&f.project, "implementer", "s1");
    let big = "o".repeat(crate::harness::transcript::items::RESULT_LIMIT * 2);
    append(
        &transcript,
        &[
            typed("u1", "Fix the login bug"),
            continuation("u2"),
            line(serde_json::json!({"type": "assistant", "uuid": "a1",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash",
                                         "input": {"command": "cargo test"}}]}})),
            line(serde_json::json!({"type": "user", "uuid": "r1",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1",
                                         "content": big}]}})),
        ]
        .concat(),
    );
    let p = &f.project_name;
    let path = format!("/v1/agents/{p}/login/implementer/transcript");

    assert_eq!(get(&f.config, &path, None).0, 401);

    let (status, body) = get(&f.config, &path, Some(&reader));
    assert_eq!(status, 200, "{body}");
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["version"], 1);
    assert_eq!(page["harness"], "claude-code");
    let kinds: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["user", "continuation", "tool"]);
    assert_eq!(page["before"], serde_json::Value::Null);
    let tool = &page["items"][2];
    assert_eq!(tool["input"], "cargo test");
    assert_eq!(tool["result"]["truncated"], true);

    let full = tool["result"]["full"].as_str().unwrap();
    let (status, whole) = get(
        &f.config,
        &format!("{path}/result?ref={}", full.replace(':', "%3A")),
        Some(&reader),
    );
    assert_eq!((status, whole.len()), (200, big.len()));

    let (_, newest) = get(&f.config, &format!("{path}?limit=1"), Some(&reader));
    let newest: serde_json::Value = serde_json::from_str(&newest).unwrap();
    assert_eq!(newest["items"][0]["id"], "toolu_1");
    let before = newest["before"].as_str().unwrap();
    let (_, older) = get(
        &f.config,
        &format!("{path}?before={before}&limit=5"),
        Some(&reader),
    );
    let older: serde_json::Value = serde_json::from_str(&older).unwrap();
    let ids: Vec<&str> = older["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["u1", "u2"]);

    for (bad, status) in [
        (format!("{path}?limit=0"), 400),
        (format!("{path}?limit=x"), 400),
        (format!("/v1/agents/{p}/login/reviewer/transcript"), 404),
        (format!("/v1/agents/{p}/%2E%2E/implementer/transcript"), 404),
        (format!("{path}/result"), 400),
        (format!("{path}/result?ref=0%3Atoolu_9"), 404),
    ] {
        assert_eq!(get(&f.config, &bad, Some(&reader)).0, status, "{bad}");
    }
}

#[test]
fn a_watched_agents_new_items_and_session_change_are_streamed() {
    let mut f = fixture();
    f.config.transcript_poll = Duration::from_millis(100);
    let token = pair(&f.config, "reader");
    let transcript = register_conversation(&f.project, "implementer", "s1");
    append(&transcript, &typed("u1", "before the watch"));
    let server = start(f.config.clone());
    let p = &f.project_name;

    let (_, page) = get(
        &f.config,
        &format!("/v1/agents/{p}/login/implementer/transcript"),
        Some(&token),
    );
    let page: serde_json::Value = serde_json::from_str(&page).unwrap();
    append(&transcript, &continuation("u2"));
    let events = connect(
        server.0.addr(),
        &format!(
            "/v1/events?watch={p}%2Flogin%2Fimplementer&after={}",
            page["after"].as_str().unwrap()
        ),
        &token,
    );
    until(&events, |l| l == "event: transcript");
    let data = until(&events, |l| l.starts_with("data: "));
    let data: serde_json::Value =
        serde_json::from_str(data[0].strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(data["agent"], "implementer");
    assert_eq!(data["reset"], false);
    let ids: Vec<&str> = data["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["u2"],
        "what was appended after the page, though before the watch"
    );

    let restarted = register_conversation(&f.project, "implementer", "s2");
    append(&restarted, &typed("n1", "a new session"));
    until(&events, |l| l == "event: transcript");
    let data = until(&events, |l| l.starts_with("data: "));
    let data: serde_json::Value =
        serde_json::from_str(data[0].strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(data["reset"], true);
    assert_eq!(data["items"][0]["id"], "n1");
    assert_eq!(data["before"], serde_json::Value::Null);
}

#[test]
fn a_conversation_that_appears_after_the_watch_began_is_sent_whole() {
    let mut f = fixture();
    f.config.transcript_poll = Duration::from_millis(100);
    let token = pair(&f.config, "reader");
    let transcript = register_conversation(&f.project, "implementer", "s1");
    let server = start(f.config.clone());
    let p = &f.project_name;
    let path = format!("/v1/agents/{p}/login/implementer/transcript");
    assert_eq!(
        get(&f.config, &path, Some(&token)).0,
        404,
        "not started yet"
    );

    let events = connect(
        server.0.addr(),
        &format!("/v1/events?watch={p}%2Flogin%2Fimplementer"),
        &token,
    );
    until(&events, |l| l == "event: snapshot");
    append(
        &transcript,
        &[typed("u1", "the brief"), continuation("u2")].concat(),
    );
    until(&events, |l| l == "event: transcript");
    let data = until(&events, |l| l.starts_with("data: "));
    let data: serde_json::Value =
        serde_json::from_str(data[0].strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(data["reset"], true);
    let ids: Vec<&str> = data["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["u1", "u2"], "the opening the client never paged");
}
