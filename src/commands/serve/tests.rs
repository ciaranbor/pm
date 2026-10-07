use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

use super::routes::{Reply, route};
use super::*;
use crate::commands::feat_status::feat_status;
use crate::state::devices::Devices;
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;
use crate::testing::TestServer;

pub(super) struct Fixture {
    _dir: TempDir,
    pub server: TestServer,
    pub project: PathBuf,
    pub project_name: String,
    pub config: Config,
}

pub(super) fn fixture() -> Fixture {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
    let mut config = Config::new(
        TestServer::registry_dir(&project),
        Devices::path(dir.path()),
        server.name(),
    );
    config.idle_poll = Duration::from_millis(100);
    config.watched_poll = Duration::from_millis(100);
    config.min_gap = Duration::ZERO;
    Fixture {
        _dir: dir,
        server,
        project,
        project_name,
        config,
    }
}

pub(super) fn pair(config: &Config, name: &str) -> String {
    Devices::update(&config.devices, |d| d.pair(name)).unwrap()
}

fn request<'a>(
    method: &'a str,
    path: &'a str,
    query: &'a str,
    authorization: Option<&'a str>,
    body: &'a str,
) -> super::routes::Request<'a> {
    super::routes::Request {
        method,
        path,
        query,
        authorization,
        if_match: None,
        body,
        too_long: false,
    }
}

/// The status and body `path` gets with `token`.
fn get(config: &Config, path: &str, token: Option<&str>) -> (u16, String) {
    let authorization = token.map(|t| format!("Bearer {t}"));
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    match route(
        config,
        "",
        &request("GET", path, query, authorization.as_deref(), ""),
    )
    .reply
    {
        Reply::Body { status, body, .. } => (status, body),
        Reply::Events { .. } => (200, "<events>".into()),
    }
}

#[test]
fn a_request_without_a_live_token_is_refused() {
    let f = fixture();
    let reader = pair(&f.config, "reader");
    let revoked = pair(&f.config, "gone");
    crate::commands::serve_revoke::revoke(&f.config.devices, "gone").unwrap();

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

/// A raw HTTP client of `addr`, its response lines sent to the receiver
/// as they arrive.
fn connect(addr: SocketAddr, path: &str, token: &str) -> mpsc::Receiver<String> {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\r\n"
    )
    .unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(|l| l.ok()) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    rx
}

/// Lines from `lines` up to and including the first that `wanted`.
fn until(lines: &mpsc::Receiver<String>, wanted: impl Fn(&str) -> bool) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = lines
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("timed out; saw {seen:#?}"));
        let done = wanted(&line);
        seen.push(line);
        if done {
            return seen;
        }
    }
}

struct Running(Arc<Server>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.stop();
    }
}

fn start(config: Config) -> Running {
    let server = Server::bind(config, 0).unwrap();
    let running = Arc::clone(&server);
    std::thread::spawn(move || running.run());
    Running(server)
}

#[test]
fn an_event_stream_sends_changes_transitions_and_heartbeats_and_every_reply_its_version() {
    let mut f = fixture();
    f.config.heartbeat = Duration::from_millis(500);
    let token = pair(&f.config, "reader");
    feat_status(
        &f.project,
        "login",
        Progress::Blocked,
        Some("standing"),
        None,
    )
    .unwrap();
    let server = start(f.config.clone());

    let version = format!("Pm-Version: {}", crate::version::VERSION);
    let refused = until(&connect(server.0.addr(), "/v1/snapshot", "nope"), |l| {
        l.is_empty()
    });
    assert!(refused[0].contains("401"), "{refused:#?}");
    assert!(
        refused.iter().any(|l| l.eq_ignore_ascii_case(&version)),
        "{refused:#?}"
    );

    let events = connect(server.0.addr(), "/v1/events", &token);
    let opening = until(&events, |l| l.starts_with("data: "));
    assert_eq!(opening[0], "HTTP/1.1 200 OK");
    assert!(opening.contains(&"Content-Type: text/event-stream".to_string()));
    assert!(opening.contains(&version), "{opening:#?}");
    assert!(opening.contains(&"event: snapshot".to_string()));

    let summary = paths::summary_path(&f.project, "login");
    std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
    std::fs::write(&summary, "Adds login\n").unwrap();
    feat_status(&f.project, "login", Progress::Ready, None, None).unwrap();
    let changed = until(&events, |l| l.starts_with("event: transition"));
    assert!(
        changed
            .iter()
            .any(|l| l.starts_with("data: ") && l.contains(r#""progress":"ready""#)),
        "{changed:#?}"
    );
    let transition = until(&events, |l| l.starts_with("data: "));
    let transition: serde_json::Value =
        serde_json::from_str(transition[0].strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(transition["project"], f.project_name.as_str());
    assert_eq!(transition["scope"], "login");
    assert_eq!(transition["kind"], "ready");

    until(&events, |l| l == ": heartbeat");
}

/// Register `agent` in `login` on Claude Code with session `session_id`,
/// recording the session's transcript path as the SessionStart hook does.
/// Returns that path; nothing is written there.
fn register_conversation(project: &std::path::Path, agent: &str, session_id: &str) -> PathBuf {
    use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
    use crate::state::runtime::{self, SessionPath};
    let transcript = project.join(format!("{session_id}.jsonl"));
    let agents_dir = paths::agents_dir(project);
    let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
    registry.register(
        agent,
        AgentEntry {
            agent_type: AgentType::Agent,
            session_id: session_id.to_string(),
            window_name: agent.to_string(),
            active: true,
            agent_definition: None,
            harness: crate::harness::Harness::ClaudeCode,
            spawned_at: None,
        },
    );
    registry.save(&agents_dir, "login").unwrap();
    runtime::write_session_path(
        project,
        "login",
        agent,
        SessionPath::Transcript,
        Some(&transcript),
    )
    .unwrap();
    transcript
}

fn line(value: serde_json::Value) -> String {
    format!("{value}\n")
}

fn typed(uuid: &str, text: &str) -> String {
    line(
        serde_json::json!({"type": "user", "uuid": uuid, "promptSource": "typed",
                            "message": {"role": "user", "content": text}}),
    )
}

fn continuation(uuid: &str) -> String {
    line(
        serde_json::json!({"type": "user", "uuid": uuid, "isMeta": true,
        "message": {"role": "user",
                    "content": "Stop hook feedback:\nYou have new messages from reviewer."}}),
    )
}

fn append(path: &std::path::Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .unwrap();
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

const VAPID: &str = "the-servers-vapid-key";

/// The status and body `method` on `path` gets with `token` and `body`.
pub(super) fn call(
    config: &Config,
    method: &str,
    path: &str,
    token: &str,
    body: &str,
) -> (u16, String) {
    let authorization = format!("Bearer {token}");
    match route(
        config,
        VAPID,
        &request(method, path, "", Some(&authorization), body),
    )
    .reply
    {
        Reply::Body { status, body, .. } => (status, body),
        Reply::Events { .. } => (200, "<events>".into()),
    }
}

/// A device's push keys: the secret it decrypts with, and the
/// subscription JSON it registers.
fn subscriber(endpoint: &str) -> (web_push_native::p256::SecretKey, [u8; 16], String) {
    use base64ct::{Base64UrlUnpadded, Encoding};
    use web_push_native::p256::elliptic_curve::sec1::ToEncodedPoint;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).unwrap();
    let secret = web_push_native::p256::SecretKey::from_slice(&bytes).unwrap();
    let mut auth = [0u8; 16];
    getrandom::fill(&mut auth).unwrap();
    let p256dh = secret.public_key().to_encoded_point(false);
    let json = serde_json::json!({
        "endpoint": endpoint,
        "keys": {
            "p256dh": Base64UrlUnpadded::encode_string(p256dh.as_bytes()),
            "auth": Base64UrlUnpadded::encode_string(&auth),
        },
    });
    (secret, auth, json.to_string())
}

fn stored_push(config: &Config, device: &str) -> Option<crate::state::devices::Push> {
    Devices::load(&config.devices).unwrap().devices[device]
        .push
        .clone()
}

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
        !super::push::key_path(&f.config.devices).exists(),
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

/// One request a push service received, and how it answered.
struct Received {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A push service on loopback answering each request with the next of
/// `statuses`; its URL, and the requests it received.
fn push_service(statuses: Vec<u16>) -> (String, mpsc::Receiver<Received>) {
    push_service_answering(
        statuses
            .into_iter()
            .map(|s| format!("HTTP/1.1 {s} X\r\nContent-Length: 0\r\n\r\n"))
            .collect(),
    )
}

/// [`push_service`], answering each request with the next raw response.
fn push_service_answering(responses: Vec<String>) -> (String, mpsc::Receiver<Received>) {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/push/abc", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (stream, response) in listener.incoming().zip(responses) {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(": ") {
                    headers.push((name.to_string(), value.to_string()));
                }
            }
            let received = Received {
                headers,
                body: Vec::new(),
            };
            let length: usize = received
                .header("content-length")
                .map_or(0, |l| l.parse().unwrap());
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
            let _ = tx.send(Received { body, ..received });
        }
    });
    (url, rx)
}

#[test]
fn a_transition_is_pushed_encrypted_to_each_subscriber_until_its_service_drops_it() {
    let mut f = fixture();
    f.config.push = PushPolicy::local();
    let phone = pair(&f.config, "phone");
    pair(&f.config, "unsubscribed");
    let (url, received) = push_service(vec![201, 410]);
    let (secret, auth, subscription) = subscriber(&url);
    let push = super::push::subscription(&subscription, &PushPolicy::local()).unwrap();
    Devices::update(&f.config.devices, |d| {
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
    let auth = web_push_native::Auth::clone_from_slice(&auth);
    let message = web_push_native::decrypt(pushed.body, &secret, &auth).unwrap();
    let message: serde_json::Value = serde_json::from_slice(&message).unwrap();
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
    feat_status(&f.project, "login", Progress::Ready, None, None).unwrap();
    received.recv_timeout(Duration::from_secs(15)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while stored_push(&f.config, "phone").is_some() {
        assert!(Instant::now() < deadline, "a 410 drops the subscription");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(received.try_recv().is_err(), "one push per transition");
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
    let push = super::push::subscription(&subscription, &PushPolicy::local()).unwrap();
    Devices::update(&f.config.devices, |d| {
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
    )
    .unwrap();
    received.recv_timeout(Duration::from_secs(15)).unwrap();
    let summary = paths::summary_path(&f.project, "login");
    std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
    std::fs::write(&summary, "Adds login\n").unwrap();
    feat_status(&f.project, "login", Progress::Ready, None, None).unwrap();
    received.recv_timeout(Duration::from_secs(15)).unwrap();
    assert!(
        followed.try_recv().is_err(),
        "the redirect's target is never sent to"
    );
}

#[test]
fn a_stored_subscription_the_policy_refuses_is_dropped() {
    let f = fixture();
    pair(&f.config, "phone");
    let (_, _, subscription) = subscriber("https://tailnet-service.ts.net/up");
    let push = super::push::subscription(&subscription, &PushPolicy::local()).unwrap();
    Devices::update(&f.config.devices, |d| {
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

/// The kind of the next transition `events` sends.
fn next_transition(events: &mpsc::Receiver<String>) -> String {
    until(events, |l| l == "event: transition");
    let data = until(events, |l| l.starts_with("data: "));
    let transition: serde_json::Value =
        serde_json::from_str(data[0].strip_prefix("data: ").unwrap()).unwrap();
    transition["kind"].as_str().unwrap().to_string()
}

/// Wait until `server` has read `n` snapshots since it started.
fn until_reads(server: &Running, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while server.0.reads.load(std::sync::atomic::Ordering::SeqCst) < n {
        assert!(Instant::now() < deadline, "never read {n} snapshots");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `server` with an event stream open, once it has read the snapshot the
/// stream opening woke it for.
fn watched(server: &Running, token: &str) -> mpsc::Receiver<String> {
    let events = connect(server.0.addr(), "/v1/events", token);
    until(&events, |l| l.starts_with("data: "));
    until_reads(server, 1);
    events
}

#[test]
fn a_wake_reads_a_change_pm_made_without_waiting_for_the_poll() {
    let mut f = fixture();
    f.config.idle_poll = Duration::from_secs(600);
    f.config.watched_poll = Duration::from_secs(600);
    let token = pair(&f.config, "reader");
    let server = start(f.config.clone());
    let events = watched(&server, &token);

    block(&f);
    wake(&f.config.devices);

    assert_eq!(next_transition(&events), "blocked");
}

#[test]
fn an_open_stream_polls_often_for_what_pm_did_not_do() {
    let mut f = fixture();
    f.config.idle_poll = Duration::from_secs(600);
    let token = pair(&f.config, "reader");
    let server = start(f.config.clone());
    let events = watched(&server, &token);

    block(&f);

    assert_eq!(next_transition(&events), "blocked");
}

#[test]
fn wakes_closer_together_than_the_gap_make_one_read() {
    let mut f = fixture();
    f.config.idle_poll = Duration::from_secs(600);
    f.config.watched_poll = Duration::from_secs(600);
    f.config.min_gap = Duration::from_secs(2);
    let token = pair(&f.config, "reader");
    let server = start(f.config.clone());
    let events = watched(&server, &token);

    block(&f);
    wake(&f.config.devices);
    // Past the wake's settling, so without the gap it would read `blocked`.
    std::thread::sleep(Duration::from_millis(500));
    let summary = paths::summary_path(&f.project, "login");
    std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
    std::fs::write(&summary, "Adds login\n").unwrap();
    feat_status(&f.project, "login", Progress::Ready, None, None).unwrap();
    wake(&f.config.devices);

    assert_eq!(next_transition(&events), "ready", "blocked was never read");
    assert_eq!(server.0.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
}

fn block(f: &Fixture) {
    feat_status(&f.project, "login", Progress::Blocked, Some("why?"), None).unwrap();
}

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
    assert_eq!(post(&typist, "keys", r#"{"keys":["Down","Enter"]}"#).0, 200);
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

#[test]
fn open_dialogs_are_served_and_an_answer_reaches_the_hook_of_the_one_it_names() {
    use crate::harness::Harness;
    use crate::state::runtime::{self, DialogRecord, Waiting, WaitingKind};
    let f = fixture();
    let token = pair(&f.config, "phone");
    register_conversation(&f.project, "implementer", "s1");
    let agent = format!("/v1/agents/{}/login/implementer/dialog", f.project_name);
    let get_one = || get(&f.config, &agent, Some(&token));
    let get_all = || {
        let (status, body) = get(&f.config, &format!("{agent}s"), Some(&token));
        assert_eq!(status, 200);
        let all: serde_json::Value = serde_json::from_str(&body).unwrap();
        all["dialogs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let post =
        |body: &serde_json::Value| call(&f.config, "POST", &agent, &token, &body.to_string());
    let start = chrono::Utc::now();
    let open = |command: &str, pid: u32, after: i64| {
        let payload = serde_json::json!({
            "hook_event_name": "PermissionRequest", "tool_name": "Bash",
            "tool_input": {"command": command}
        });
        let (mut dialog, reply_context) = Harness::ClaudeCode.dialog(&payload).unwrap();
        dialog.since = start + chrono::Duration::seconds(after);
        let record = DialogRecord {
            dialog,
            pid: Some(pid),
            reply_context,
        };
        runtime::write_dialog(&f.project, "login", "implementer", &record).unwrap();
        record
    };
    let waiting = Waiting::now(WaitingKind::Permission, Some("Bash: cargo publish".into()));
    runtime::write_waiting(&f.project, "login", "implementer", &waiting).unwrap();

    let dead = open("cargo publish", u32::MAX / 2, 0);
    assert_eq!(get_one().0, 404, "its hook is dead");
    assert_eq!(get_all(), Vec::<String>::new());
    assert_eq!(
        post(&serde_json::json!({"id": dead.dialog.id, "choice": "allow"})),
        (
            409,
            serde_json::json!({"error": "the dialog's hook is gone", "refused": "gone"})
                .to_string()
        )
    );

    // This process stands in for both hooks.
    let older = open("cargo publish", std::process::id(), 1);
    let newer = open("git push", std::process::id(), 2);
    let (status, body) = get_one();
    assert_eq!(status, 200);
    let served: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(served["id"], older.dialog.id.as_str(), "the oldest");
    assert_eq!(served["tool"], "Bash");
    assert_eq!(served["detail"], "cargo publish");
    assert_eq!(served["choices"][1]["id"], "deny");
    assert_eq!(served.get("reply_context"), None);
    assert_eq!(served.get("pid"), None);
    assert_eq!(
        get_all(),
        [older.dialog.id.clone(), newer.dialog.id.clone()]
    );

    let unknown = post(&serde_json::json!({"id": "0123abcd", "choice": "allow"}));
    assert_eq!(unknown.0, 404, "{}", unknown.1);
    let id = newer.dialog.id.clone();
    assert_eq!(
        post(&serde_json::json!({"id": id, "choice": "maybe"})).0,
        400
    );
    assert_eq!(post(&serde_json::json!({"id": id})).0, 400);

    let project = f.project.clone();
    let taken = id.clone();
    let hook = std::thread::spawn(move || {
        for _ in 0..250 {
            if let Some(answer) =
                runtime::take_answer(&project, "login", "implementer", &taken).unwrap()
            {
                runtime::close_dialog(&project, "login", "implementer", &answer.id, true).unwrap();
                return Some(answer);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    });
    let bearer = format!("Bearer {token}");
    let body = serde_json::json!({"id": id, "choice": "deny", "message": "dry-run first"});
    let body = body.to_string();
    let handled = route(
        &f.config,
        VAPID,
        &request("POST", &agent, "", Some(&bearer), &body),
    );
    let answer = hook.join().unwrap().unwrap();
    assert_eq!(handled.reply.status(), 200);
    assert_eq!(answer.choice, "deny");
    assert_eq!(answer.message.as_deref(), Some("dry-run first"));
    let logged = handled.detail.unwrap();
    assert!(
        logged.starts_with("dialog deny message sha256:"),
        "{logged}"
    );
    assert!(!logged.contains("dry-run"), "{logged}");
    assert_eq!(
        get_all(),
        std::slice::from_ref(&older.dialog.id),
        "answered"
    );
    let again = post(&serde_json::json!({"id": id, "choice": "allow"}));
    assert_eq!(again.0, 409);
    assert!(again.1.contains(r#""refused":"answered""#), "{}", again.1);
}

#[test]
fn notes_are_saved_only_from_the_version_they_were_read_at() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, projects_dir, name) = server.setup_project_no_tmux(dir.path());
    let config = Config::new(projects_dir, Devices::path(dir.path()), None);
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
    let over = "x".repeat(super::notes::MAX_TEXT + 1);
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

    crate::commands::serve_revoke::revoke(&f.config.devices, "phone").unwrap();

    until(&revoked, |l| l == "event: revoked");
    until(&revoked, |l| l == "0");
    block(&f);
    wake(&f.config.devices);
    assert_eq!(next_transition(&kept), "blocked");
}

#[test]
fn a_device_unpairs_itself_and_only_itself() {
    let f = fixture();
    let phone = pair(&f.config, "phone");
    let tablet = pair(&f.config, "tablet");

    assert_eq!(call(&f.config, "GET", "/v1/pairing", &phone, "").0, 405);
    assert_eq!(call(&f.config, "DELETE", "/v1/pairing", &phone, "").0, 204);

    let devices = Devices::load(&f.config.devices).unwrap();
    assert_eq!(devices.devices.keys().collect::<Vec<_>>(), ["tablet"]);
    assert_eq!(call(&f.config, "DELETE", "/v1/pairing", &phone, "").0, 401);
    assert_eq!(get(&f.config, "/v1/snapshot", Some(&tablet)).0, 200);
}
