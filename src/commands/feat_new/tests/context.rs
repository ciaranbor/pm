use super::*;
use std::io::Cursor;

#[test]
fn resolve_context_dash_reads_stdin() {
    let body = "line one\nline two\n";
    let resolved = resolve_context_from("-", Cursor::new(body)).unwrap();
    assert_eq!(resolved, body);
}

#[test]
fn resolve_context_literal_does_not_touch_stdin() {
    // Passing a non-empty stdin proves the literal path never reads it.
    let resolved = resolve_context_from("just a string", Cursor::new("STDIN")).unwrap();
    assert_eq!(resolved, "just a string");
}

#[test]
fn resolve_context_file_path_still_reads_file() {
    let dir = tempdir().unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "file body").unwrap();
    let resolved = resolve_context_from(brief.to_str().unwrap(), Cursor::new("STDIN")).unwrap();
    assert_eq!(resolved, "file body");
}

#[test]
fn resolve_stdin_context_dash_reads_stdin() {
    let body = "multi\nline\nbrief\n";
    let resolved = resolve_stdin_context_from(Some("-"), Cursor::new(body)).unwrap();
    assert_eq!(resolved.as_deref(), Some(body));
}

#[test]
fn resolve_stdin_context_literal_unchanged_and_no_file_resolution() {
    let dir = tempdir().unwrap();
    let brief = dir.path().join("brief.md");
    std::fs::write(&brief, "file body").unwrap();
    // Unlike resolve_context, a file path is left as the literal string.
    let resolved =
        resolve_stdin_context_from(Some(brief.to_str().unwrap()), Cursor::new("STDIN")).unwrap();
    assert_eq!(resolved.as_deref(), Some(brief.to_str().unwrap()));
}

#[test]
fn resolve_stdin_context_none_is_none() {
    let resolved = resolve_stdin_context_from(None, Cursor::new("STDIN")).unwrap();
    assert_eq!(resolved, None);
}

#[test]
fn feat_new_with_text_context_enqueues_message_to_brief_agents() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    feat_new(&FeatNewParams {
        context: Some("Implement login page per issue #42"),
        workflow: Some("implement-and-review"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();

    // No TASK.md on disk any more.
    assert!(!project_path.join("login").join("TASK.md").exists());

    // The context is queued as a message in the implementer's inbox
    // (the sole brief_agents recipient of `implement-and-review`).
    let messages_dir = paths::messages_dir(&project_path);
    let summaries = crate::messages::list(&messages_dir, "login", "implementer", None).unwrap();
    assert_eq!(summaries.len(), 1);
    let msg = crate::messages::read_at(
        &messages_dir,
        "login",
        "implementer",
        &summaries[0].sender,
        summaries[0].index,
    )
    .unwrap()
    .unwrap();
    assert!(msg.body.contains("Implement login page per issue #42"));
}

#[test]
fn feat_new_with_file_context_reads_file() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    // Create a temp file with context content
    let brief_path = dir.path().join("brief.md");
    std::fs::write(&brief_path, "# Login Feature\nBuild the login page").unwrap();

    feat_new(&FeatNewParams {
        context: Some(brief_path.to_str().unwrap()),
        workflow: Some("implement-and-review"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();

    // No TASK.md; content is queued to the brief recipient's inbox.
    assert!(!project_path.join("login").join("TASK.md").exists());
    let messages_dir = paths::messages_dir(&project_path);
    let summaries = crate::messages::list(&messages_dir, "login", "implementer", None).unwrap();
    assert_eq!(summaries.len(), 1);
    let msg = crate::messages::read_at(
        &messages_dir,
        "login",
        "implementer",
        &summaries[0].sender,
        summaries[0].index,
    )
    .unwrap()
    .unwrap();
    assert!(msg.body.contains("# Login Feature\nBuild the login page"));
}

#[test]
fn feat_new_with_context_stores_resolved_content_in_state() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    // Pass a file path as context — state should store the file contents, not the path
    let brief_path = dir.path().join("brief.md");
    std::fs::write(&brief_path, "resolved file content").unwrap();

    feat_new(&FeatNewParams {
        context: Some(brief_path.to_str().unwrap()),
        workflow: Some("implement-and-review"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();

    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.context, "resolved file content");
}

#[test]
fn feat_new_with_context_creates_claude_window() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

    feat_new(&FeatNewParams {
        context: Some("Build the login page"),
        workflow: Some("implement-and-review"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();

    // implement-and-review spawns the full team (implementer + reviewer).
    // 3 windows: reused window :0 (implementer) + reviewer + hook window.
    let output =
        tmux::list_windows(server.name(), &tmux::session_name(&project_name, "login")).unwrap();
    assert_eq!(output, 3);
}
