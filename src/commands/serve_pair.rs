//! `pm serve pair`: give a device a token of its own. The QR code carries
//! what the phone app needs to connect, as JSON: `{"url", "device",
//! "token"}`, `url` being where the transport serves `pm serve`.

use std::path::Path;

use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;
use serde::Serialize;

use crate::error::{PmError, Result};
use crate::state::devices::{Devices, Scope};
use crate::tailscale;

#[derive(Serialize)]
pub struct Pairing {
    pub url: String,
    pub device: String,
    pub token: String,
}

impl Pairing {
    /// The pairing as a QR code drawn for a dark terminal: light modules
    /// in the foreground colour, so the code reads the right way round.
    pub fn qr(&self) -> Result<String> {
        let payload = serde_json::to_string(self)?;
        let code = QrCode::new(payload.as_bytes())
            .map_err(|e| PmError::Serve(format!("cannot encode the pairing: {e}")))?;
        Ok(code
            .render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .quiet_zone(true)
            .build())
    }
}

/// Pair `device` with `scopes` in the devices file at `devices`. `url`
/// defaults to this machine's tailnet name over HTTPS, as `tailscale serve`
/// serves it.
pub fn pair(devices: &Path, device: &str, scopes: &[Scope], url: Option<&str>) -> Result<Pairing> {
    let url = match url {
        Some(url) => url.trim_end_matches('/').to_string(),
        None => tailscale::dns_name()
            .map(|name| format!("https://{name}"))
            .ok_or_else(|| {
                PmError::Serve(
                    "tailscale reports no name for this machine; pass --url with the address the phone reaches pm serve at".into(),
                )
            })?,
    };
    let mut paired = Devices::load(devices)?;
    let token = paired.pair(device, scopes)?;
    paired.save(devices)?;
    Ok(Pairing {
        url,
        device: device.to_string(),
        token,
    })
}
