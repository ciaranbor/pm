//! The `tailscale` CLI, which `pm serve pair` asks for this machine's
//! tailnet name.

use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "Self")]
    me: Peer,
}

#[derive(Deserialize)]
struct Peer {
    #[serde(rename = "DNSName")]
    dns_name: String,
}

/// This machine's MagicDNS name (`mac.tailnet.ts.net`), when `tailscale`
/// is installed and connected.
pub fn dns_name() -> Option<String> {
    let output = Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let status: Status = serde_json::from_slice(&output.stdout).ok()?;
    Some(status.me.dns_name.trim_end_matches('.').to_string()).filter(|n| !n.is_empty())
}
