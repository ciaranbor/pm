//! The `pm serve` API end to end, one module per endpoint group; the
//! fixture, clients and transcript builders they share live here.

mod devices;
mod dialog;
mod events;
mod input;
mod mode;
mod notes;
mod push;
mod routes;
mod transcript;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

use super::routes::{Reply, route};
use super::*;
use super::{notes as serve_notes, push as serve_push};
use crate::commands::feat_status::feat_status;
use crate::state::devices::Devices;
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;
use crate::testing::{TestServer, agent_entry};

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
    let (project, projects_dir, project_name) =
        server.setup_project_with_feature(dir.path(), "login");
    let mut config = Config::new(
        projects_dir.clone(),
        ServeFiles::new(dir.path().into(), dir.path().into()),
        server.name(),
    );
    config.idle_poll = Duration::from_millis(100);
    config.watched_poll = Duration::from_millis(100);
    config.min_gap = Duration::ZERO;
    config.push_grace = Duration::ZERO;
    Fixture {
        _dir: dir,
        server,
        project,
        project_name,
        config,
    }
}

pub(super) fn pair(config: &Config, name: &str) -> String {
    Devices::update(&config.files, |d| d.pair(name)).unwrap()
}

pub(super) fn request<'a>(
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

/// Register `agent` in `login` on Claude Code with session `session_id`,
/// recording the session's transcript path as the SessionStart hook does.
/// Returns that path; nothing is written there.
fn register_conversation(project: &std::path::Path, agent: &str, session_id: &str) -> PathBuf {
    use crate::state::agent::{AgentEntry, AgentRegistry};
    use crate::state::runtime::{self, SessionPath};
    let transcript = project.join(format!("{session_id}.jsonl"));
    let agents_dir = paths::agents_dir(project);
    let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
    registry.register(
        agent,
        AgentEntry {
            session_id: session_id.to_string(),
            ..agent_entry(agent)
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

fn append(path: &std::path::Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .unwrap();
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

fn block(f: &Fixture) {
    feat_status(
        &f.project,
        "login",
        Progress::Blocked,
        Some("why?"),
        None,
        crate::testing::NO_SERVER,
    )
    .unwrap();
}
