//! Web Push (RFC 8030) of each transition to every device that registered
//! a subscription, so the phone hears of one without reaching the tailnet:
//! the push service (a UnifiedPush distributor's server) is on the public
//! internet. The message is encrypted to the device's keys (RFC 8291) and
//! carries only which scope entered which kind; the app fetches the rest
//! over the tailnet when opened.
//!
//! The server signs each push with its VAPID key (RFC 8292), which some
//! push services (FCM among them) require. The key is made on first use
//! and kept beside the devices file; a device registers against its public
//! half, so replacing the key strands every subscription.
//!
//! Where a push may go is the [`policy`]'s to say; a stored subscription
//! it refuses (made before a host left `push_hosts`) is dropped, as is one
//! a push service answers 404 or 410 for. A worker thread sends, so a slow
//! push service never holds up the poller.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use base64ct::{Base64UrlUnpadded, Encoding};
use serde::Serialize;
use web_push_native::jwt_simple::algorithms::{ECDSAP256KeyPairLike, ES256KeyPair};
use web_push_native::{Auth, WebPushBuilder, p256};

use crate::commands::attention::AttentionKind;
use crate::commands::attention::transition::Transition;
use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::state::devices::{Devices, Push};

use super::log;

pub mod policy;

use policy::Policy;

const KEY_NAME: &str = "vapid.pem";

/// Who to contact about this server's pushes, as RFC 8292 asks; push
/// services don't check it, and pm has no address of its own.
const CONTACT: &str = "mailto:pm-serve@localhost";

/// How long a push service keeps a message for a device it can't reach.
const TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// The VAPID key file beside the devices file at `devices`.
pub(super) fn key_path(devices: &Path) -> PathBuf {
    devices.with_file_name(KEY_NAME)
}

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

/// What a push says: the transition without its detail, which may be long
/// and need not leave the tailnet.
#[derive(Debug, Serialize)]
struct Message<'a> {
    project: &'a str,
    scope: &'a str,
    kind: AttentionKind,
    agent: Option<&'a str>,
}

/// Sends each batch of transitions it is given, on a thread of its own.
pub(super) struct Pusher {
    batches: Sender<Vec<Transition>>,
}

impl Pusher {
    pub(super) fn start(devices: PathBuf, key: ES256KeyPair, policy: Policy) -> Self {
        let (batches, rx) = mpsc::channel::<Vec<Transition>>();
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
            for transitions in rx {
                if let Err(e) = deliver(&agent, &devices, &key, &policy, &transitions) {
                    log(&format!("push: {e}"));
                }
            }
        });
        Self { batches }
    }

    pub(super) fn send(&self, transitions: Vec<Transition>) {
        if !transitions.is_empty() {
            let _ = self.batches.send(transitions);
        }
    }
}

fn deliver(
    agent: &ureq::Agent,
    devices: &Path,
    key: &ES256KeyPair,
    policy: &Policy,
    transitions: &[Transition],
) -> Result<()> {
    let paired = Devices::load(devices)?;
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
        for transition in transitions {
            match send(agent, key, push, transition) {
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

/// Push `transition` to `push`, returning the push service's status.
fn send(
    agent: &ureq::Agent,
    key: &ES256KeyPair,
    push: &Push,
    transition: &Transition,
) -> std::result::Result<u16, String> {
    let message = Message {
        project: &transition.project,
        scope: &transition.scope,
        kind: transition.attention.kind,
        agent: transition.attention.agent.as_deref(),
    };
    let body = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
    let mut request = builder(push)?
        .with_vapid(key, CONTACT)
        .build(body)
        .map_err(|e| e.to_string())?;
    request
        .headers_mut()
        .insert("Urgency", http::HeaderValue::from_static("high"));
    let response = agent.run(request).map_err(|e| e.to_string())?;
    Ok(response.status().as_u16())
}

/// Drop `name`'s subscription, unless it has registered another since.
fn forget(devices: &Path, name: &str, gone: &Push) -> Result<()> {
    Devices::update(devices, |paired| {
        if let Some(device) = paired.devices.get_mut(name)
            && device.push.as_ref() == Some(gone)
        {
            device.push = None;
        }
        Ok(())
    })
}
