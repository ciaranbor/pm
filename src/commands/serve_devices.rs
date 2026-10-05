//! `pm serve devices`: the devices paired with `pm serve`.

use std::path::Path;

use crate::error::Result;
use crate::state::devices::Devices;

/// One line per paired device: its name, when it was paired, and what it
/// was granted.
pub fn devices(devices: &Path) -> Result<Vec<String>> {
    let paired = Devices::load(devices)?;
    let width = paired.devices.keys().map(String::len).max().unwrap_or(0);
    Ok(paired
        .devices
        .iter()
        .map(|(name, d)| {
            let grants: Vec<String> = d.grants.iter().map(ToString::to_string).collect();
            let granted = if grants.is_empty() {
                String::new()
            } else {
                format!("  granted {}", grants.join(", "))
            };
            format!(
                "{name:width$}  paired {}{granted}",
                d.paired.format("%Y-%m-%d %H:%M UTC")
            )
        })
        .collect())
}
