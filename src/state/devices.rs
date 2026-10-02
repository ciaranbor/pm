//! The devices paired with `pm serve`, each with its own bearer token and
//! scopes. Only a token's SHA-256 is stored: a token is 256 random bits, so
//! a plain hash is as good as a slow one, and the file leaking gives away no
//! token. The file is machine-local, under the config dir's `serve/`, which
//! the registry's `.gitignore` block names, and readable only by the user.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;

/// The config-dir-relative dir `pm serve` keeps its machine-local files in.
pub const DIR_NAME: &str = "serve";
const FILE_NAME: &str = "devices.toml";

/// What a device's token lets it do.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Read the snapshot, events, summaries and agent screens.
    Read,
    /// Send input to agents.
    Input,
    /// Start, stop and delete features and agents.
    Lifecycle,
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::Read => "read",
            Self::Input => "input",
            Self::Lifecycle => "lifecycle",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub token_sha256: String,
    pub scopes: Vec<Scope>,
    pub paired: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Devices {
    #[serde(default)]
    pub devices: BTreeMap<String, Device>,
}

impl Devices {
    /// The devices file under the pm config dir `config_dir`.
    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join(DIR_NAME).join(FILE_NAME)
    }

    /// The devices in `path`; none when it doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, toml::to_string(self)?.as_bytes())?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    /// Pair a device named `name` with `scopes`, returning its token, the
    /// only time it is known.
    pub fn pair(&mut self, name: &str, scopes: &[Scope]) -> Result<String> {
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
        let mut scopes = scopes.to_vec();
        scopes.sort();
        scopes.dedup();
        self.devices.insert(
            name.to_string(),
            Device {
                token_sha256: digest(&token),
                scopes,
                paired: Utc::now(),
            },
        );
        Ok(token)
    }

    /// Forget `name`'s token. Whether it was paired.
    pub fn revoke(&mut self, name: &str) -> bool {
        self.devices.remove(name).is_some()
    }

    /// The device `token` belongs to.
    pub fn authenticate(&self, token: &str) -> Option<(&str, &Device)> {
        let hash = digest(token);
        self.devices
            .iter()
            .find(|(_, d)| same(d.token_sha256.as_bytes(), hash.as_bytes()))
            .map(|(name, d)| (name.as_str(), d))
    }
}

fn digest(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
        let path = Devices::path(dir.path());
        let mut devices = Devices::load(&path).unwrap();
        let token = devices.pair("pixel", &[Scope::Input, Scope::Read]).unwrap();
        devices.save(&path).unwrap();

        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(!stored.contains(&token));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let devices = Devices::load(&path).unwrap();
        let (name, device) = devices.authenticate(&token).unwrap();
        assert_eq!(name, "pixel");
        assert_eq!(device.scopes, [Scope::Read, Scope::Input]);
        assert!(devices.authenticate(&format!("{token}0")).is_none());

        let mut devices = devices;
        assert!(devices.revoke("pixel"));
        assert!(devices.authenticate(&token).is_none());
    }

    #[test]
    fn a_name_pairs_once() {
        let mut devices = Devices::default();
        let first = devices.pair("pixel", &[Scope::Read]).unwrap();
        assert!(devices.pair("pixel", &[Scope::Read]).is_err());
        let other = devices.pair("tablet", &[Scope::Read]).unwrap();
        assert_ne!(first, other);
    }
}
