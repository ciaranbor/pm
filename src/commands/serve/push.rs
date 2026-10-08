//! Web Push (RFC 8030) of each transition to every device that registered
//! a subscription, so the phone hears of one without reaching the tailnet:
//! the push service (a UnifiedPush distributor's server) is on the public
//! internet. The message is encrypted to the device's keys (RFC 8291) and
//! carries only which scope entered which kind; the app fetches the rest
//! over the tailnet when opened. An episode's end is pushed too, so the
//! app withdraws its alert while closed; the [`queue`] decides when each
//! goes. An end goes at normal urgency: it posts nothing, and FCM
//! deprioritizes an app whose high-priority messages show nothing.
//!
//! The server signs each push with its VAPID key (RFC 8292), which some
//! push services (FCM among them) require. The key is made on first use
//! and kept with the devices file ([`ServeFiles`]); a device registers against its public
//! half, so replacing the key strands every subscription.
//!
//! Where a push may go is the [`policy`]'s to say; a stored subscription
//! it refuses (made before a host left `push_hosts`) is dropped, as is one
//! a push service answers 404 or 410 for. A worker thread sends, so a slow
//! push service never holds up the poller.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use base64ct::{Base64UrlUnpadded, Encoding};
use serde::Serialize;
use web_push_native::jwt_simple::algorithms::{ECDSAP256KeyPairLike, ES256KeyPair};
use web_push_native::{Auth, WebPushBuilder, p256};

use crate::commands::attention::AttentionKind;
use crate::commands::attention::transition::{Ended, Transition};
use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::state::devices::{Devices, Push};
use crate::state::serve_files::ServeFiles;

use super::log;

pub mod policy;
mod queue;

use policy::Policy;
use queue::{Outgoing, Queue};

/// Who to contact about this server's pushes, as RFC 8292 asks; push
/// services don't check it, and pm has no address of its own.
const CONTACT: &str = "mailto:pm-serve@localhost";

/// How long a push service keeps a message for a device it can't reach.
const TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// The VAPID key at `path`, made and saved first if there is none.
pub(super) fn vapid_key(path: &Path) -> Result<ES256KeyPair> {
    let invalid = |e: web_push_native::jwt_simple::Error| {
        PmError::Serve(format!("VAPID key {}: {e}", path.display()))
    };
    match std::fs::read_to_string(path) {
        Ok(pem) => ES256KeyPair::from_pem(&pem).map_err(invalid),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = ES256KeyPair::generate();
            write_atomic(path, key.to_pem().map_err(invalid)?.as_bytes())?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            Ok(key)
        }
        Err(e) => Err(e.into()),
    }
}

/// The public half of `key` as a subscriber passes it to its push service:
/// an uncompressed P-256 point, base64url.
pub(super) fn public_key(key: &ES256KeyPair) -> String {
    Base64UrlUnpadded::encode_string(&key.key_pair().public_key().to_bytes_uncompressed())
}

/// A subscription as the app sends it: the Web Push `PushSubscription`
/// JSON shape. Checked before it is stored, so a push can't fail on it.
pub(super) fn subscription(body: &str, policy: &Policy) -> std::result::Result<Push, String> {
    #[derive(serde::Deserialize)]
    struct Keys {
        p256dh: String,
        auth: String,
    }
    #[derive(serde::Deserialize)]
    struct Subscription {
        endpoint: String,
        keys: Keys,
    }
    let s: Subscription = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if let Some(refusal) = policy.refusal(&s.endpoint) {
        return Err(refusal);
    }
    let push = Push {
        endpoint: s.endpoint,
        p256dh: s.keys.p256dh,
        auth: s.keys.auth,
    };
    builder(&push)?;
    Ok(push)
}

fn builder(push: &Push) -> std::result::Result<WebPushBuilder, String> {
    let endpoint = push.endpoint.parse().map_err(|_| "endpoint is no URL")?;
    let p256dh = Base64UrlUnpadded::decode_vec(push.p256dh.trim_end_matches('='))
        .ok()
        .and_then(|bytes| p256::PublicKey::from_sec1_bytes(&bytes).ok())
        .ok_or("keys.p256dh is no P-256 public key")?;
    let auth = Base64UrlUnpadded::decode_vec(push.auth.trim_end_matches('='))
        .ok()
        .filter(|bytes| bytes.len() == 16)
        .ok_or("keys.auth is no 16-byte secret")?;
    Ok(
        WebPushBuilder::new(endpoint, p256dh, Auth::clone_from_slice(&auth))
            .with_valid_duration(TTL),
    )
}

/// What a push says: a transition without its detail, which may be long
/// and need not leave the tailnet, or an episode's end. An end has no
/// `kind`, so an app that knows only transitions drops it.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Message<'a> {
    Begun {
        project: &'a str,
        scope: &'a str,
        kind: AttentionKind,
        agent: Option<&'a str>,
    },
    Ended {
        project: &'a str,
        scope: &'a str,
        ended: AttentionKind,
        agent: Option<&'a str>,
    },
}

impl<'a> From<&'a Outgoing> for Message<'a> {
    fn from(push: &'a Outgoing) -> Self {
        match push {
            Outgoing::Begun(t) => Self::Begun {
                project: &t.project,
                scope: &t.scope,
                kind: t.attention.kind,
                agent: t.attention.agent.as_deref(),
            },
            Outgoing::Ended(e) => Self::Ended {
                project: &e.project,
                scope: &e.scope,
                ended: e.kind,
                agent: e.agent.as_deref(),
            },
        }
    }
}

enum Order {
    Push {
        begun: Vec<Transition>,
        ended: Vec<Ended>,
    },
    /// Send everything held now, then answer.
    Flush(Sender<()>),
}

/// Sends what each poll found, on a thread of its own.
pub(super) struct Pusher {
    orders: Sender<Order>,
}

impl Pusher {
    pub(super) fn start(
        devices: ServeFiles,
        key: ES256KeyPair,
        policy: Policy,
        grace: Duration,
    ) -> Self {
        let (orders, rx) = mpsc::channel::<Order>();
        std::thread::spawn(move || {
            let config = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(30)))
                .max_redirects(0)
                .proxy(None)
                .build();
            let agent = ureq::Agent::with_parts(
                config,
                ureq::unversioned::transport::DefaultConnector::new(),
                policy.resolver(),
            );
            let mut queue = Queue::new(grace);
            loop {
                let order = match queue.next_due() {
                    Some(due) => {
                        match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                            Ok(order) => Some(order),
                            Err(RecvTimeoutError::Timeout) => None,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    None => match rx.recv() {
                        Ok(order) => Some(order),
                        Err(_) => return,
                    },
                };
                let now = Instant::now();
                let (mut pushes, flushed) = match order {
                    Some(Order::Push { begun, ended }) => (queue.take(begun, ended, now), None),
                    Some(Order::Flush(done)) => (queue.drain(), Some(done)),
                    None => (Vec::new(), None),
                };
                pushes.extend(queue.due(now));
                if !pushes.is_empty()
                    && let Err(e) = deliver(&agent, &devices, &key, &policy, &pushes)
                {
                    log(&format!("push: {e}"));
                }
                if let Some(done) = flushed {
                    let _ = done.send(());
                }
            }
        });
        Self { orders }
    }

    pub(super) fn send(&self, begun: Vec<Transition>, ended: Vec<Ended>) {
        if !begun.is_empty() || !ended.is_empty() {
            let _ = self.orders.send(Order::Push { begun, ended });
        }
    }

    /// Send every held transition now, returning once it has gone.
    pub(super) fn flush(&self) {
        let (done, sent) = mpsc::channel();
        if self.orders.send(Order::Flush(done)).is_ok() {
            let _ = sent.recv();
        }
    }
}

fn deliver(
    agent: &ureq::Agent,
    devices: &ServeFiles,
    key: &ES256KeyPair,
    policy: &Policy,
    pushes: &[Outgoing],
) -> Result<()> {
    let paired = Devices::load(&devices.devices())?;
    for (name, device) in &paired.devices {
        let Some(push) = device.push.as_ref() else {
            continue;
        };
        if let Some(refusal) = policy.refusal(&push.endpoint) {
            log(&format!(
                "{name} push: subscription refused ({refusal}); dropped"
            ));
            forget(devices, name, push)?;
            continue;
        }
        for message in pushes {
            match send(agent, key, push, message) {
                Ok(status) if (200..300).contains(&status) => {}
                Ok(404 | 410) => {
                    log(&format!("{name} push: subscription gone; dropped"));
                    forget(devices, name, push)?;
                    break;
                }
                Ok(status) => log(&format!("{name} push: {status}")),
                Err(e) => log(&format!("{name} push: {e}")),
            }
        }
    }
    Ok(())
}

/// Push `message` to `push`, returning the push service's status.
fn send(
    agent: &ureq::Agent,
    key: &ES256KeyPair,
    push: &Push,
    message: &Outgoing,
) -> std::result::Result<u16, String> {
    let urgency = match message {
        Outgoing::Begun(_) => "high",
        Outgoing::Ended(_) => "normal",
    };
    let body = serde_json::to_vec(&Message::from(message)).map_err(|e| e.to_string())?;
    let mut request = builder(push)?
        .with_vapid(key, CONTACT)
        .build(body)
        .map_err(|e| e.to_string())?;
    request
        .headers_mut()
        .insert("Urgency", http::HeaderValue::from_static(urgency));
    let response = agent.run(request).map_err(|e| e.to_string())?;
    Ok(response.status().as_u16())
}

/// Drop `name`'s subscription, unless it has registered another since.
fn forget(devices: &ServeFiles, name: &str, gone: &Push) -> Result<()> {
    Devices::update(devices, |paired| {
        if let Some(device) = paired.devices.get_mut(name)
            && device.push.as_ref() == Some(gone)
        {
            device.push = None;
        }
        Ok(())
    })
}
