//! `pm serve devices`: the devices paired with `pm serve`.

use std::path::Path;

use crate::error::Result;
use crate::state::devices::Devices;

/// One line per paired device: its name, scopes and when it was paired.
pub fn devices(devices: &Path) -> Result<Vec<String>> {
    let paired = Devices::load(devices)?;
    let width = paired.devices.keys().map(String::len).max().unwrap_or(0);
    Ok(paired
        .devices
        .iter()
        .map(|(name, d)| {
            let scopes: Vec<String> = d.scopes.iter().map(ToString::to_string).collect();
            format!(
                "{name:width$}  {:24}  paired {}",
                scopes.join(","),
                d.paired.format("%Y-%m-%d %H:%M UTC")
            )
        })
        .collect())
}
