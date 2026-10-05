//! `pm serve devices`: the devices paired with `pm serve`.

use std::path::Path;

use crate::error::Result;
use crate::state::devices::Devices;

/// One line per paired device: its name and when it was paired.
pub fn devices(devices: &Path) -> Result<Vec<String>> {
    let paired = Devices::load(devices)?;
    let width = paired.devices.keys().map(String::len).max().unwrap_or(0);
    Ok(paired
        .devices
        .iter()
        .map(|(name, d)| {
            format!(
                "{name:width$}  paired {}",
                d.paired.format("%Y-%m-%d %H:%M UTC")
            )
        })
        .collect())
}
