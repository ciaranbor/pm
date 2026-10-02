//! `pm serve`: a read-only HTTP API over pm state for the phone app,
//! reached through a transport (`tailscale serve`) that forwards to it.
//! README, "Remote access", has the user-facing setup.
//!
//! It listens on 127.0.0.1 only, never another address: whatever reaches
//! it does so through the transport, so every request arrives on loopback
//! and the bearer token is the only proof of who sent it — no request is
//! trusted for being local. Each request runs on a thread of its own; the
//! event streams hold theirs for as long as the client stays.
//!
//! One poller thread reads the [`attention`] snapshot every few seconds,
//! sends it to every event stream when it changed, and judges it against
//! the last ([`Watch`]) for transitions: the rule tmux alerts by, plus an
//! agent dying. The first snapshot, read as the server starts, is the
//! baseline, so a restart reports nothing that was already so. Each
//! transition also goes to every subscribed device as a Web Push
//! (`push`), which reaches a phone off the tailnet. The poller
//! also re-executes the binary once it is replaced ([`Binary`]), so an
//! upgrade reaches a server launchd keeps running.
//!
//! Every request is logged to stderr with the device whose token it
//! carried; launchd sends that to `serve.log` in the devices' dir.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};

use crate::error::{PmError, Result};

use super::attention::{self, transition::Watch};
use super::reexec::Binary;

mod events;
mod push;
mod routes;
mod transcript;

use events::Hub;
use push::Pusher;
use routes::Reply;

/// The most of a request body read; a push subscription is well under it.
const MAX_BODY: u64 = 16 * 1024;

pub const DEFAULT_PORT: u16 = 7764;

/// What a server serves, and how often it looks.
#[derive(Debug, Clone)]
pub struct Config {
    pub projects_dir: PathBuf,
    /// The paired devices' file ([`Devices::path`](crate::state::devices::Devices::path)).
    pub devices: PathBuf,
    pub tmux_server: Option<String>,
    /// How often the snapshot is read for changes.
    pub poll: Duration,
    /// How long an event stream may go without sending anything.
    pub heartbeat: Duration,
    /// How often a stream watching an agent reads its conversation.
    pub transcript_poll: Duration,
}

impl Config {
    pub fn new(projects_dir: PathBuf, devices: PathBuf, tmux_server: Option<&str>) -> Self {
        Self {
            projects_dir,
            devices,
            tmux_server: tmux_server.map(str::to_string),
            poll: Duration::from_secs(3),
            heartbeat: Duration::from_secs(25),
            transcript_poll: Duration::from_secs(1),
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
        Ok(Arc::new(Self {
            http,
            vapid: push::public_key(&key),
            pusher: Pusher::start(config.devices.clone(), key),
            config,
            hub: Hub::new(&snapshot)?,
            watch: Mutex::new(Watch::start(&snapshot)),
            stopped: AtomicBool::new(false),
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
    }

    fn poll(&self) {
        let binary = Binary::current();
        while !self.stopped.load(Ordering::SeqCst) {
            std::thread::sleep(self.config.poll);
            let read = attention::all(
                &self.config.projects_dir,
                self.config.tmux_server.as_deref(),
            );
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
            &mut std::io::Read::take(request.as_reader(), MAX_BODY),
            &mut body,
        );
        let handled = match read {
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
            },
        };
        let device = handled.device.as_deref().unwrap_or("-");
        log(&format!(
            "{device} {method} {path} {}",
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
                    .with_header(header("Content-Type", content_type));
                if status == 401 {
                    response = response.with_header(header("WWW-Authenticate", "Bearer"));
                }
                request.respond(response)
            }
            Reply::Events(watch) => {
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

/// Serve on loopback `port` until the process ends.
pub fn serve(config: Config, port: u16) -> Result<()> {
    let server = Server::bind(config, port)?;
    log(&format!("listening on http://{}", server.addr()));
    server.run();
    Ok(())
}

#[cfg(test)]
mod tests;
