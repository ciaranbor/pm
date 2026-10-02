//! `pm serve revoke`: forget a device's token. A running server checks
//! every request against the devices file, so the token stops working at
//! once.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::devices::Devices;

pub fn revoke(devices: &Path, device: &str) -> Result<()> {
    let mut paired = Devices::load(devices)?;
    if !paired.revoke(device) {
        return Err(PmError::Serve(format!(
            "no device named {device} is paired"
        )));
    }
    paired.save(devices)
}
