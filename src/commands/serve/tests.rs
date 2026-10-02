use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

use super::routes::{Reply, route};
use super::*;
use crate::commands::feat_status::feat_status;
use crate::state::devices::{Devices, Scope};
use crate::state::feature::Progress;
use crate::state::paths;
use crate::testing::TestServer;

struct Fixture {
    _dir: TempDir,
    server: TestServer,
    project: PathBuf,
    project_name: String,
    config: Config,
}

fn fixture() -> Fixture {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
    let config = Config::new(
        TestServer::registry_dir(&project),
        Devices::path(dir.path()),
        server.name(),
    );
    Fixture {
        _dir: dir,
        server,
        project,
        project_name,
        config,
    }
}

fn pair(config: &Config, name: &str, scopes: &[Scope]) -> String {
    let mut devices = Devices::load(&config.devices).unwrap();
    let token = devices.pair(name, scopes).unwrap();
    devices.save(&config.devices).unwrap();
    token
}

/// The status and body `path` gets with `token`.
fn get(config: &Config, path: &str, token: Option<&str>) -> (u16, String) {
    let authorization = token.map(|t| format!("Bearer {t}"));
    match route(config, "GET", path, authorization.as_deref()).reply {
        Reply::Body { status, body, .. } => (status, body),
        Reply::Events => (200, "<events>".into()),
    }
}

#[test]
fn a_request_without_a_live_token_with_the_read_scope_is_refused() {
    let f = fixture();
    let reader = pair(&f.config, "reader", &[Scope::Read]);
    let typist = pair(&f.config, "typist", &[Scope::Input]);
    let revoked = pair(&f.config, "gone", &[Scope::Read]);
    let mut devices = Devices::load(&f.config.devices).unwrap();
    devices.revoke("gone");
    devices.save(&f.config.devices).unwrap();

    let status = |path: &str, token: Option<&str>| get(&f.config, path, token).0;
    assert_eq!(status("/v1/snapshot", None), 401);
    assert_eq!(status("/v1/nowhere", None), 401, "no token learns no paths");
    assert_eq!(status("/v1/snapshot", Some("not-a-token")), 401);
    assert_eq!(status("/v1/snapshot", Some(&revoked)), 401);
    assert_eq!(status("/v1/snapshot", Some(&typist)), 403);
    assert_eq!(status("/v1/events", Some(&typist)), 403);
    assert_eq!(status("/v1/snapshot", Some(&reader)), 200);

    let handled = route(
        &f.config,
        "GET",
        "/v1/snapshot",
        Some(&format!("Bearer {reader}")),
    );
    assert_eq!(handled.device.as_deref(), Some("reader"));
    let lowercase = route(
        &f.config,
        "GET",
        "/v1/snapshot",
        Some(&format!("bearer {reader}")),
    );
    assert_eq!(
        lowercase.reply.status(),
        200,
        "the scheme is case-insensitive"
    );
}

#[test]
fn the_endpoints_serve_the_snapshot_a_summary_and_an_agents_screen() {
    let f = fixture();
    let token = pair(&f.config, "reader", &[Scope::Read]);
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
    let (status, screen) = get(&format!("/v1/agents/{p}/login/implementer/screen"));
    assert_eq!(status, 200);
    assert!(screen.contains("sleep 999"), "{screen}");

    for missing in [
        "/v1/features/nope/login/summary".to_string(),
        format!("/v1/features/{p}/search/summary"),
        format!("/v1/features/{p}/%2E%2E/summary"),
        format!("/v1/features/{p}/..%2F..%2Fconfig/summary"),
        format!("/v1/agents/{p}/login/reviewer/screen"),
        format!("/v1/agents/{p}/%2E%2E/implementer/screen"),
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
fn an_event_stream_sends_changes_transitions_and_heartbeats() {
    let mut f = fixture();
    f.config.poll = Duration::from_millis(100);
    f.config.heartbeat = Duration::from_millis(500);
    let token = pair(&f.config, "reader", &[Scope::Read]);
    feat_status(
        &f.project,
        "login",
        Progress::Blocked,
        Some("standing"),
        None,
    )
    .unwrap();
    let server = start(f.config.clone());

    let events = connect(server.0.addr(), "/v1/events", &token);
    let opening = until(&events, |l| l.starts_with("data: "));
    assert_eq!(opening[0], "HTTP/1.1 200 OK");
    assert!(opening.contains(&"Content-Type: text/event-stream".to_string()));
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
