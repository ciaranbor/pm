use super::*;

#[test]
fn notes_are_saved_only_from_the_version_they_were_read_at() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, name) = server.setup_project_no_tmux(dir.path());
    let config = Config::new(
        projects_dir,
        ServeFiles::new(dir.path().into(), dir.path().into()),
        None,
    );
    let bearer = format!("Bearer {}", pair(&config, "phone"));
    let path = format!("/v1/projects/{name}/notes");
    let send = |method: &str, if_match: Option<&str>, body: &str| {
        let mut req = request(method, &path, "", Some(&bearer), body);
        req.if_match = if_match;
        match route(&config, "", &req).reply {
            Reply::Body {
                status, body, etag, ..
            } => (status, body, etag),
            Reply::Events { .. } => unreachable!(),
        }
    };

    let (status, body, empty) = send("GET", None, "");
    assert_eq!(
        (status, body.as_str()),
        (200, ""),
        "no notes yet read as empty"
    );
    let empty = empty.unwrap();

    assert_eq!(send("PUT", None, "lost?\n").0, 428);
    let over = "x".repeat(serve_notes::MAX_TEXT + 1);
    let (status, body, _) = send("PUT", Some(&empty), &over);
    assert_eq!(status, 413);
    assert!(body.contains("pm notes"), "{body}");
    let mut unread = request("PUT", &path, "", Some(&bearer), "");
    unread.too_long = true;
    let refused = route(&config, "", &unread);
    assert_eq!(refused.device.as_deref(), Some("phone"));
    let Reply::Body { status, body, .. } = refused.reply else {
        unreachable!()
    };
    assert_eq!(status, 413);
    assert!(
        body.contains("pm notes"),
        "notes past what the server reads get the same refusal: {body}"
    );
    let (status, _, saved) = send("PUT", Some(&format!("\"{empty}\"")), "from the phone\n");
    assert_eq!(status, 200);
    let saved = saved.unwrap();
    assert_eq!(
        std::fs::read_to_string(paths::notes_path(&project)).unwrap(),
        "from the phone\n"
    );
    assert_eq!(send("GET", None, "").2.as_deref(), Some(saved.as_str()));

    std::fs::write(paths::notes_path(&project), "from the terminal\n").unwrap();
    let (status, body, current) = send("PUT", Some(&format!("\"{saved}\"")), "stale\n");
    assert_eq!(status, 409);
    let refused: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(refused["refused"], "changed");
    assert_eq!(refused["text"], "from the terminal\n");
    assert_eq!(refused["version"].as_str(), current.as_deref());
    assert_eq!(
        std::fs::read_to_string(paths::notes_path(&project)).unwrap(),
        "from the terminal\n"
    );

    let other = |path: &str| {
        route(&config, "", &request("GET", path, "", Some(&bearer), ""))
            .reply
            .status()
    };
    assert_eq!(other("/v1/projects/nope/notes"), 404);
    assert_eq!(other(&format!("/v1/projects/{name}/..%2Fconfig.toml")), 404);
}
