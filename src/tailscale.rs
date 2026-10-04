//! The `tailscale` CLI: this machine's tailnet name, which `pm serve pair`
//! puts in a pairing, and whether `tailscale serve` forwards the tailnet's
//! HTTPS to `pm serve`'s port ([`Serving`]), which `pm serve install` sets
//! up when it can.
//!
//! `tailscale serve` needs MagicDNS and HTTPS certificates enabled for the
//! tailnet; without them it prints a consent URL and blocks, so it is run
//! only once [`check`] reads them as enabled.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "Self")]
    me: Option<Peer>,
    #[serde(rename = "CertDomains", default)]
    cert_domains: Option<Vec<String>>,
    #[serde(rename = "CurrentTailnet", default)]
    tailnet: Option<Tailnet>,
}

#[derive(Deserialize)]
struct Peer {
    #[serde(rename = "DNSName", default)]
    dns_name: String,
}

#[derive(Deserialize)]
struct Tailnet {
    #[serde(rename = "MagicDNSEnabled", default)]
    magic_dns: bool,
}

/// `tailscale serve status --json`, as much of it as says what port 443
/// does.
#[derive(Deserialize, Default)]
struct ServeConfig {
    #[serde(rename = "TCP", default)]
    tcp: HashMap<String, TcpHandler>,
    #[serde(rename = "Web", default)]
    web: HashMap<String, WebServer>,
}

#[derive(Deserialize)]
struct TcpHandler {
    #[serde(rename = "HTTPS", default)]
    https: bool,
    #[serde(rename = "TCPForward", default)]
    tcp_forward: String,
}

#[derive(Deserialize)]
struct WebServer {
    #[serde(rename = "Handlers", default)]
    handlers: HashMap<String, WebHandler>,
}

#[derive(Deserialize)]
struct WebHandler {
    #[serde(rename = "Proxy", default)]
    proxy: String,
    #[serde(rename = "Path", default)]
    path: String,
    #[serde(rename = "Text", default)]
    text: String,
}

/// What `tailscale serve` does for `pm serve`'s port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Serving {
    NotInstalled,
    /// Installed, but not connected to a tailnet.
    NotRunning,
    /// The tailnet has MagicDNS or HTTPS certificates off.
    HttpsDisabled,
    /// The tailnet's port 443 here goes somewhere else, described.
    PortTaken(String),
    /// Nothing serves port 443, so `tailscale serve` can.
    NotServing,
    /// The tailnet's HTTPS reaches `pm serve` at this URL.
    Serving(String),
}

impl Serving {
    /// One line for the user: what this means, and what to do if anything.
    pub fn advice(&self, port: u16) -> String {
        match self {
            Self::NotInstalled => format!(
                "tailscale is not installed; install it (https://tailscale.com/download), or forward your own transport to 127.0.0.1:{port}"
            ),
            Self::NotRunning => "tailscale is not connected; run `tailscale up`".into(),
            Self::HttpsDisabled => "the tailnet has MagicDNS or HTTPS certificates off; enable both at https://login.tailscale.com/admin/dns, then run `pm serve install` again".into(),
            Self::PortTaken(what) => format!(
                "tailscale already serves port 443 here ({what}); pm left it alone — to serve pm there, `tailscale serve --https=443 off`, then run `pm serve install` again"
            ),
            Self::NotServing => format!(
                "tailscale does not serve pm; run `pm serve install`, or `tailscale serve --bg {port}`"
            ),
            Self::Serving(url) => format!("tailscale serves pm at {url}"),
        }
    }
}

/// The JSON `tailscale` prints for `args`. `Err(true)` when it isn't
/// installed, `Err(false)` when it fails or prints something else.
fn json<T: for<'de> Deserialize<'de>>(args: &[&str]) -> Result<T, bool> {
    let output = match Command::new("tailscale").args(args).output() {
        Err(e) => return Err(e.kind() == std::io::ErrorKind::NotFound),
        Ok(o) if !o.status.success() => return Err(false),
        Ok(o) => o,
    };
    serde_json::from_slice(&output.stdout).map_err(|_| false)
}

/// This machine's MagicDNS name (`mac.tailnet.ts.net`), when `tailscale`
/// is installed and connected.
pub fn dns_name() -> Option<String> {
    json::<Status>(&["status", "--json"])
        .ok()
        .and_then(|s| name(&s))
}

fn name(status: &Status) -> Option<String> {
    let name = status.me.as_ref()?.dns_name.trim_end_matches('.');
    Some(name.to_string()).filter(|n| !n.is_empty())
}

/// What `tailscale serve` does for `port` now.
pub fn check(port: u16) -> Serving {
    let status = match json::<Status>(&["status", "--json"]) {
        Ok(status) => status,
        Err(true) => return Serving::NotInstalled,
        Err(false) => return Serving::NotRunning,
    };
    let serve = json::<ServeConfig>(&["serve", "status", "--json"]).unwrap_or_default();
    classify(&status, &serve, port)
}

fn classify(status: &Status, serve: &ServeConfig, port: u16) -> Serving {
    let Some(name) = name(status).filter(|_| status.backend_state == "Running") else {
        return Serving::NotRunning;
    };
    if let Some(taken) = serve.tcp.get("443") {
        let root = serve
            .web
            .get(&format!("{name}:443"))
            .and_then(|web| web.handlers.get("/"));
        return match root {
            Some(h) if h.proxy.is_empty() => {
                Serving::PortTaken(if h.path.is_empty() && !h.text.is_empty() {
                    "a text reply".into()
                } else {
                    format!("files at {}", h.path)
                })
            }
            Some(h) if proxies_to(&h.proxy, port) => Serving::Serving(format!("https://{name}")),
            Some(h) => Serving::PortTaken(format!("a proxy to {}", h.proxy)),
            None if !taken.tcp_forward.is_empty() => {
                Serving::PortTaken(format!("TCP forwarded to {}", taken.tcp_forward))
            }
            None if taken.https => Serving::PortTaken("HTTPS without a handler at /".into()),
            None => Serving::PortTaken("TCP".into()),
        };
    }
    let certs = status.cert_domains.as_ref().is_some_and(|d| !d.is_empty());
    let magic_dns = status.tailnet.as_ref().is_some_and(|t| t.magic_dns);
    if !certs || !magic_dns {
        return Serving::HttpsDisabled;
    }
    Serving::NotServing
}

fn proxies_to(proxy: &str, port: u16) -> bool {
    let target = proxy.strip_prefix("http://").unwrap_or(proxy);
    let target = target.trim_end_matches('/');
    ["127.0.0.1", "localhost"]
        .iter()
        .any(|host| target == format!("{host}:{port}"))
        || target == port.to_string()
}

/// Run `tailscale serve --bg <port>`, giving up after `timeout`. The error
/// is what it printed.
pub fn serve(port: u16, timeout: Duration) -> Result<(), String> {
    let mut child = Command::new("tailscale")
        .args(["serve", "--bg", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let mut said = String::new();
                for stream in [
                    child.stdout.take().map(|s| Box::new(s) as Box<dyn Read>),
                    child.stderr.take().map(|s| Box::new(s) as Box<dyn Read>),
                ]
                .into_iter()
                .flatten()
                {
                    let _ = { stream }.read_to_string(&mut said);
                }
                return if status.success() {
                    Ok(())
                } else {
                    Err(said.trim().to_string())
                };
            }
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`tailscale serve --bg {port}` did not finish in {}s",
                    timeout.as_secs()
                ));
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "mac.tailnet.ts.net";

    fn status(backend: &str, certs: Option<&[&str]>) -> Status {
        let json = serde_json::json!({
            "BackendState": backend,
            "Self": { "DNSName": format!("{NAME}.") },
            "CertDomains": certs,
            "CurrentTailnet": { "MagicDNSEnabled": true },
        });
        serde_json::from_value(json).unwrap()
    }

    fn serve(json: serde_json::Value) -> ServeConfig {
        serde_json::from_value(json).unwrap()
    }

    fn proxied(proxy: &str) -> ServeConfig {
        serve(serde_json::json!({
            "TCP": { "443": { "HTTPS": true } },
            "Web": { format!("{NAME}:443"): { "Handlers": { "/": { "Proxy": proxy } } } },
        }))
    }

    #[test]
    fn what_tailscale_serve_does_for_pm_serves_port_is_read_from_its_status() {
        let certs: &[&str] = &[NAME];
        let running = status("Running", Some(certs));
        let empty = serve(serde_json::json!({}));

        assert_eq!(
            classify(&status("Stopped", Some(certs)), &empty, 7764),
            Serving::NotRunning
        );
        assert_eq!(
            classify(&status("Running", None), &empty, 7764),
            Serving::HttpsDisabled,
            "CertDomains null is HTTPS off"
        );
        assert_eq!(classify(&running, &empty, 7764), Serving::NotServing);
        assert_eq!(
            classify(&running, &proxied("http://127.0.0.1:7764"), 7764),
            Serving::Serving(format!("https://{NAME}"))
        );
        assert_eq!(
            classify(&running, &proxied("http://127.0.0.1:3000"), 7764),
            Serving::PortTaken("a proxy to http://127.0.0.1:3000".into())
        );
        let forwarded = serve(serde_json::json!({
            "TCP": { "443": { "TCPForward": "127.0.0.1:22" } },
        }));
        assert_eq!(
            classify(&running, &forwarded, 7764),
            Serving::PortTaken("TCP forwarded to 127.0.0.1:22".into())
        );
    }
}
