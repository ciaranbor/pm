//! The devices paired with `pm serve`, each with its own bearer token,
//! which may do everything the API offers. Only a token's SHA-256 is
//! stored: a token is 256 random bits, so a plain hash is as good as a slow
//! one, and the file leaking gives away no token. The file is
//! machine-local ([`ServeFiles`]) and readable only by the user.
//!
//! A device's Web Push subscription lives on its entry, so revoking the
//! device drops it. Every change goes through [`Devices::update`], which
//! holds a lock across the read and the write: `pm serve` registers
//! subscriptions while `pair` and `revoke` run, and a change made from a
//! stale read could bring a revoked device back.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::hash::{hex, sha256_hex};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::state::serve_files::ServeFiles;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub token_sha256: String,
    pub paired: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<Push>,
}

/// Where a device's push service takes its messages, and the keys they are
/// encrypted to (RFC 8291), base64url as the device sent them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Push {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Devices {
    #[serde(default)]
    pub devices: BTreeMap<String, Device>,
}

impl Devices {
    /// The devices in `path`; none when it doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, toml::to_string(self)?.as_bytes())?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    /// Apply `change` to the devices in `files` and save them, holding the
    /// devices' lock throughout. Nothing is saved when `change` fails.
    pub fn update<T>(files: &ServeFiles, change: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        files.create()?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(files.devices_lock())?;
        lock.lock()?;
        let path = files.devices();
        let mut devices = Self::load(&path)?;
        let out = change(&mut devices)?;
        devices.save(&path)?;
        Ok(out)
    }

    /// Pair a device named `name`, returning its token, the only time it is
    /// known.
    pub fn pair(&mut self, name: &str) -> Result<String> {
        if name.is_empty() || name.chars().any(|c| c.is_control() || c == '/') {
            return Err(PmError::Serve(format!("invalid device name {name:?}")));
        }
        if self.devices.contains_key(name) {
            return Err(PmError::Serve(format!(
                "a device named {name} is already paired; revoke it first or pick another name"
            )));
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|e| PmError::Serve(format!("no randomness for a token: {e}")))?;
        let token = hex(&bytes);
        self.devices.insert(
            name.to_string(),
            Device {
                token_sha256: digest(&token),
                paired: Utc::now(),
                push: None,
            },
        );
        Ok(token)
    }

    /// Forget `name`'s token. Whether it was paired.
    pub fn revoke(&mut self, name: &str) -> bool {
        self.devices.remove(name).is_some()
    }

    /// The name and entry of the device `token` belongs to.
    pub fn authenticate(&self, token: &str) -> Option<(&str, &Device)> {
        let hash = digest(token);
        self.devices
            .iter()
            .find(|(_, d)| same(d.token_sha256.as_bytes(), hash.as_bytes()))
            .map(|(name, d)| (name.as_str(), d))
    }

    /// Whether the token whose SHA-256 is `token_sha256` is still paired.
    pub fn holds(&self, token_sha256: &str) -> bool {
        self.devices
            .values()
            .any(|d| d.token_sha256 == token_sha256)
    }
}

fn digest(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

/// Equality that takes as long wherever the inputs first differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_paired_token_authenticates_until_revoked_and_is_never_stored() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("devices.toml");
        let mut devices = Devices::load(&path).unwrap();
        let token = devices.pair("pixel").unwrap();
        devices.save(&path).unwrap();

        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(!stored.contains(&token));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let devices = Devices::load(&path).unwrap();
        assert_eq!(devices.authenticate(&token).map(|(n, _)| n), Some("pixel"));
        assert!(devices.authenticate(&format!("{token}0")).is_none());

        let mut devices = devices;
        assert!(devices.revoke("pixel"));
        assert!(devices.authenticate(&token).is_none());
    }

    #[test]
    fn a_name_pairs_once() {
        let mut devices = Devices::default();
        let first = devices.pair("pixel").unwrap();
        assert!(devices.pair("pixel").is_err());
        let other = devices.pair("tablet").unwrap();
        assert_ne!(first, other);
    }

    #[test]
    fn a_device_paired_with_old_scopes_or_grants_still_authenticates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("devices.toml");
        let token = "ab".repeat(32);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "[devices.phone]\ntoken_sha256 = \"{}\"\nscopes = [\"read\"]\ngrants = [\"lifecycle\"]\npaired = \"2026-10-01T10:00:00Z\"\n",
                digest(&token)
            ),
        )
        .unwrap();

        let devices = Devices::load(&path).unwrap();
        assert_eq!(devices.authenticate(&token).map(|(n, _)| n), Some("phone"));
    }
}
