//! Where `pm serve` may send a push. A subscription's endpoint comes from
//! a paired device, so without a check any read-scoped token could make
//! the server POST to an https service of its choosing, on the tailnet or
//! the Mac itself. An endpoint must be https on a known push service's
//! host — Google's (FCM, which UnifiedPush's embedded distributor uses),
//! ntfy.sh, or one `[serve] push_hosts` names in the global config, for a
//! self-hosted distributor — and is checked again as it is sent: the
//! host's addresses are resolved and every one that isn't public (loopback,
//! private, link-local, CGNAT and so the tailnet, unique-local, …) is
//! dropped before connecting, so a name can't point the push inward.
//! Redirects aren't followed and no proxy is used.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ureq::config::Config as UreqConfig;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::NextTimeout;

/// The push services every server allows.
pub const KNOWN_HOSTS: [&str; 2] = ["fcm.googleapis.com", "ntfy.sh"];

#[derive(Debug, Clone)]
pub struct Policy {
    hosts: Vec<String>,
    /// Allow http and non-public addresses: a push service on loopback, in
    /// tests.
    local: bool,
}

impl Policy {
    /// The known push services, and the hosts of `extra`.
    pub fn new(extra: &[String]) -> Self {
        let hosts = KNOWN_HOSTS
            .iter()
            .map(|h| h.to_string())
            .chain(extra.iter().map(|h| h.trim().to_ascii_lowercase()))
            .filter(|h| !h.is_empty())
            .collect();
        Self {
            hosts,
            local: false,
        }
    }

    #[cfg(test)]
    pub fn local() -> Self {
        Self {
            hosts: Vec::new(),
            local: true,
        }
    }

    /// Why `endpoint` may not be pushed to, if it may not.
    pub fn refusal(&self, endpoint: &str) -> Option<String> {
        if self.local {
            return None;
        }
        let Ok(uri) = endpoint.parse::<Uri>() else {
            return Some("endpoint is no URL".into());
        };
        if uri.scheme_str() != Some("https") {
            return Some("endpoint must be an https URL".into());
        }
        let host = uri.host().unwrap_or_default().to_ascii_lowercase();
        if !self.hosts.contains(&host) {
            return Some(format!(
                "{host} is not a known push service; add it to [serve] push_hosts in pm's global config"
            ));
        }
        None
    }

    /// The resolver a push connects through: the system's, keeping only
    /// addresses this policy may reach.
    pub fn resolver(&self) -> PublicResolver {
        PublicResolver { local: self.local }
    }
}

#[derive(Debug)]
pub struct PublicResolver {
    local: bool,
}

impl Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &UreqConfig,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let resolved = DefaultResolver::default().resolve(uri, config, timeout)?;
        let mut kept = self.empty();
        for addr in resolved.iter().filter(|a| self.local || public(a.ip())) {
            kept.push(*addr);
        }
        if kept.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(kept)
    }
}

/// Whether `ip` is on the public internet.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public_v4(v4),
            None => public_v6(v6),
        },
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..20).contains(&b))
        || a >= 240)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        || (first == 0x0064 && ip.segments()[1] == 0xff9b)
        || first == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_on_a_known_or_configured_push_service_is_allowed() {
        let policy = Policy::new(&["Push.Example.org".into()]);
        for allowed in [
            "https://fcm.googleapis.com/fcm/send/abc",
            "https://ntfy.sh/upAbc?up=1",
            "https://push.example.org/up/1",
        ] {
            assert_eq!(policy.refusal(allowed), None, "{allowed}");
        }
        for refused in [
            "http://ntfy.sh/upAbc",
            "https://evil.example/fcm.googleapis.com",
            "https://fcm.googleapis.com@evil.example/",
            "https://ntfy.sh.evil.example/up",
            "https://127.0.0.1/up",
            "not a url",
        ] {
            assert!(policy.refusal(refused).is_some(), "{refused}");
        }
    }

    #[test]
    fn a_name_resolving_inward_resolves_to_nothing() {
        let resolve = |policy: &Policy| {
            policy.resolver().resolve(
                &"https://localhost:443/up".parse().unwrap(),
                &UreqConfig::default(),
                NextTimeout {
                    after: std::time::Duration::from_secs(5).into(),
                    reason: ureq::Timeout::Global,
                },
            )
        };
        let allowed = Policy::new(&["localhost".into()]);
        assert_eq!(allowed.refusal("https://localhost/up"), None);
        assert!(matches!(resolve(&allowed), Err(ureq::Error::HostNotFound)));
        assert!(resolve(&Policy::local()).is_ok());
    }

    #[test]
    fn only_public_addresses_are_reached() {
        for inward in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.88.72.77",
            "0.0.0.0",
            "::1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:100.64.0.1",
        ] {
            assert!(!public(inward.parse().unwrap()), "{inward}");
        }
        for outward in ["142.250.180.10", "159.203.148.75", "2a00:1450:4009::5f"] {
            assert!(public(outward.parse().unwrap()), "{outward}");
        }
    }
}
