//! `pm serve revoke`: forget a device's token and push subscription. A
//! running server checks every request and every push against the devices
//! file, so both stop at once; it is woken to end the device's open event
//! streams too.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::devices::Devices;

pub fn revoke(devices: &Path, device: &str) -> Result<()> {
    Devices::update(devices, |paired| {
        if paired.revoke(device) {
            Ok(())
        } else {
            Err(PmError::Serve(format!(
                "no device named {device} is paired"
            )))
        }
    })?;
    super::serve::wake(devices);
    Ok(())
}
