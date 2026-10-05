//! `pm serve`: an HTTP API over pm state for the phone app, which reads it
//! and types input into agents, reached through a transport (`tailscale
//! serve`) that forwards to it.
//! README, "Remote access", has the user-facing setup.
//!
//! It listens on 127.0.0.1 only, never another address: whatever reaches
//! it does so through the transport, so every request arrives on loopback
//! and the bearer token is the only proof of who sent it — no request is
//! trusted for being local. Each request runs on a thread of its own; the
//! event streams hold theirs for as long as the client stays.
//!
//! One poller thread reads the [`attention`] snapshot, sends it to every
//! event stream when it changed, and judges it against the last ([`Watch`])
//! for transitions: the rule tmux alerts by, plus an agent dying. The first
//! snapshot, read as the server starts, is the baseline, so a restart
//! reports nothing that was already so. Each transition also goes to every
//! subscribed device as a Web Push (`push`), which reaches a phone off the
//! tailnet. The poller also re-executes the binary once it is replaced
//! ([`Binary`]), so an upgrade reaches a server launchd keeps running.
//!
//! Nothing on the phone is urgent, so the poller is sparing: it reads the
//! snapshot every minute, and every few seconds only while an event stream
//! is open — the app on screen. A change pm makes itself needn't wait for
//! either: the background push pm commands start as they finish (`pm tmux
//! push`) also wakes the poller ([`wake`]), so the minute only bounds what
//! pm cannot see happen, such as a harness exiting. A stream opening wakes
//! it too, so the app reads a fresh snapshot. Reads stay a few seconds
//! apart however often it is woken: busy agents push on every turn.
//!
//! One server runs per pm config dir, holding a lock ([`state`]); another
//! waits for it to exit. Every request is logged to stderr with the device
//! whose token it carried; launchd sends that to `serve.log` in the
//! devices' dir.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};

use crate::error::{PmError, Result};

use super::attention::{self, transition::Watch};
use super::reexec::Binary;

mod dialog;
mod events;
mod input;
mod push;
mod routes;
pub mod state;
mod transcript;
mod wake;

use events::Hub;
use push::Pusher;
pub use push::policy::Policy as PushPolicy;
use routes::Reply;
use wake::Waker;
pub use wake::wake;

/// The response header every reply carries, naming the server's version.
const VERSION_HEADER: &str = "Pm-Version";

/// The most of a request body read: the longest text a device may send,
/// as JSON, with room for its escapes.
const MAX_BODY: u64 = 2 * input::MAX_TEXT as u64 + 1024;

pub const DEFAULT_PORT: u16 = 7764;

/// The port to listen on: `[serve] port` in the global config at
/// `config_dir`, else [`DEFAULT_PORT`].
pub fn configured_port(config_dir: &std::path::Path) -> u16 {
    crate::state::project::GlobalConfig::load(config_dir)
        .ok()
        .and_then(|c| c.serve.port)
        .unwrap_or(DEFAULT_PORT)
}

/// What a server serves, and how often it looks.
#[derive(Debug, Clone)]
pub struct Config {
    pub projects_dir: PathBuf,
    /// The paired devices' file ([`Devices::path`](crate::state::devices::Devices::path)).
    pub devices: PathBuf,
    pub tmux_server: Option<String>,
    /// How often the snapshot is read while no event stream is open.
    pub idle_poll: Duration,
    /// How often while one is.
    pub watched_poll: Duration,
    /// The least time between two reads, however often the server is woken.
    pub min_gap: Duration,
    /// How long an event stream may go without sending anything.
    pub heartbeat: Duration,
    /// How often a stream watching an agent reads its conversation.
    pub transcript_poll: Duration,
    /// Where a push may be sent.
    pub push: PushPolicy,
}

impl Config {
    pub fn new(projects_dir: PathBuf, devices: PathBuf, tmux_server: Option<&str>) -> Self {
        Self {
            projects_dir,
            devices,
            tmux_server: tmux_server.map(str::to_string),
            idle_poll: Duration::from_secs(60),
            watched_poll: Duration::from_secs(5),
            min_gap: Duration::from_secs(5),
            heartbeat: Duration::from_secs(25),
            transcript_poll: Duration::from_secs(1),
            push: PushPolicy::new(&[]),
        }
    }
}

pub struct Server {
    http: tiny_http::Server,
    config: Config,
    hub: Hub,
    pusher: Pusher,
    /// The public half of the key `pusher` signs with.
    vapid: String,
    /// What the last snapshot was judged to be, for the next.
    watch: Mutex<Watch>,
    stopped: AtomicBool,
    waker: Waker,
    /// How many snapshots the poller has read.
    #[cfg(test)]
    reads: std::sync::atomic::AtomicUsize,
}

impl Server {
    /// Listen on loopback `port` (0 picks a free one), read the baseline
    /// snapshot, and load the VAPID key, made if there is none.
    pub fn bind(config: Config, port: u16) -> Result<Arc<Self>> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let http = tiny_http::Server::http(addr)
            .map_err(|e| PmError::Serve(format!("cannot listen on {addr}: {e}")))?;
        let snapshot = attention::all(&config.projects_dir, config.tmux_server.as_deref())?;
        let key = push::vapid_key(&push::key_path(&config.devices))?;
        let waker = Waker::open(&config.devices)?;
        Ok(Arc::new(Self {
            waker,
            http,
            vapid: push::public_key(&key),
            pusher: Pusher::start(config.devices.clone(), key, config.push.clone()),
            config,
            hub: Hub::new(&snapshot)?,
            watch: Mutex::new(Watch::start(&snapshot)),
            stopped: AtomicBool::new(false),
            #[cfg(test)]
            reads: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    pub fn addr(&self) -> SocketAddr {
        self.http
            .server_addr()
            .to_ip()
            .expect("bound to an IP address")
    }

    /// Serve until [`Self::stop`].
    pub fn run(self: &Arc<Self>) {
        let poller = Arc::clone(self);
        std::thread::spawn(move || poller.poll());
        for request in self.http.incoming_requests() {
            let server = Arc::clone(self);
            std::thread::spawn(move || server.handle(request));
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.http.unblock();
        self.waker.notify();
    }

    fn poll(&self) {
        let binary = Binary::current();
        let mut last = Instant::now();
        while !self.stopped.load(Ordering::SeqCst) {
            let every = if self.hub.watched() {
                self.config.watched_poll
            } else {
                self.config.idle_poll
            };
            self.waker.wait(every);
            // Wakes meanwhile are drained into this read.
            while let Some(left) =
                (last + self.config.min_gap).checked_duration_since(Instant::now())
                && !self.stopped.load(Ordering::SeqCst)
            {
                self.waker.wait(left);
            }
            if self.stopped.load(Ordering::SeqCst) {
                return;
            }
            last = Instant::now();
            let read = attention::all(
                &self.config.projects_dir,
                self.config.tmux_server.as_deref(),
            );
            #[cfg(test)]
            self.reads.fetch_add(1, Ordering::SeqCst);
            let mut watch = self.watch.lock().unwrap_or_else(|e| e.into_inner());
            match read.and_then(|snapshot| self.hub.update(&watch, &snapshot)) {
                Ok((next, transitions)) => {
                    *watch = next;
                    self.pusher.send(transitions);
                }
                Err(e) => log(&format!("snapshot unreadable: {e}")),
            }
            drop(watch);
            if let Some(binary) = binary.as_ref().filter(|b| b.replaced()) {
                log(&format!(
                    "binary replaced; re-executing failed: {}",
                    binary.exec()
                ));
            }
        }
    }

    fn handle(&self, mut request: tiny_http::Request) {
        let authorization = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .map(|h| h.value.as_str().to_string());
        let method = request.method().as_str().to_string();
        let (path, query) = request
            .url()
            .split_once('?')
            .map_or((request.url(), ""), |(path, query)| (path, query));
        let (path, query) = (path.to_string(), query.to_string());
        let mut body = String::new();
        let read = std::io::Read::read_to_string(
            &mut std::io::Read::take(request.as_reader(), MAX_BODY + 1),
            &mut body,
        );
        let handled = match read {
            Ok(_) if body.len() as u64 > MAX_BODY => routes::Handled {
                device: None,
                reply: routes::error(413, "the body is too long"),
                detail: None,
            },
            Ok(_) => routes::route(
                &self.config,
                &self.vapid,
                &routes::Request {
                    method: &method,
                    path: &path,
                    query: &query,
                    authorization: authorization.as_deref(),
                    body: &body,
                },
            ),
            Err(_) => routes::Handled {
                device: None,
                reply: routes::error(400, "the body is not UTF-8"),
                detail: None,
            },
        };
        let device = handled.device.as_deref().unwrap_or("-");
        let detail = handled
            .detail
            .as_deref()
            .map(|d| format!(" {d}"))
            .unwrap_or_default();
        log(&format!(
            "{device} {method} {path} {}{detail}",
            handled.reply.status()
        ));
        let _ = match handled.reply {
            Reply::Body {
                status,
                content_type,
                body,
            } => {
                let mut response = tiny_http::Response::from_string(body)
                    .with_status_code(status)
                    .with_header(header("Content-Type", content_type))
                    .with_header(header(VERSION_HEADER, crate::version::VERSION));
                if status == 401 {
                    response = response.with_header(header("WWW-Authenticate", "Bearer"));
                }
                request.respond(response)
            }
            Reply::Events(watch) => {
                self.waker.notify();
                let mut writer = request.into_writer();
                let watch = watch.map(|w| (*w, self.config.transcript_poll));
                events::stream(&mut writer, &self.hub, self.config.heartbeat, watch)
            }
        };
    }
}

fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("a valid header")
}

fn log(line: &str) {
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    eprintln!("{now} {line}");
}

/// Serve on loopback `port` until the process ends, once no other server
/// of the config dir `config_dir` runs.
pub fn serve(config: Config, config_dir: &std::path::Path, port: u16) -> Result<()> {
    let lock = match state::lock(config_dir)? {
        Some(lock) => lock,
        None => {
            let holder = state::State::load(config_dir).map_or("?".into(), |s| s.pid.to_string());
            log(&format!(
                "waiting for the running server (pid {holder}) to exit"
            ));
            state::wait(config_dir)?
        }
    };
    let server = Server::bind(config, port)?;
    state::State::now(server.addr().port()).save(config_dir)?;
    log(&format!("listening on http://{}", server.addr()));
    server.run();
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests;
