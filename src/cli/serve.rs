use clap::Subcommand;

#[derive(Subcommand)]
pub enum ServeCommands {
    /// Pair a device: print a QR code of the server's URL and a new token
    Pair {
        /// The device's name, used to revoke it and in the request log
        #[arg(long, default_value = "phone")]
        name: String,
        /// The URL the device reaches the server at (default: this machine's
        /// tailnet name over HTTPS, as `tailscale serve` serves it)
        #[arg(long)]
        url: Option<String>,
    },
    /// List paired devices
    Devices,
    /// Revoke a device's token
    Revoke {
        /// The device's name
        device: String,
    },
    /// Run `pm serve` as a launchd LaunchAgent (macOS), at login and across
    /// restarts, and set up `tailscale serve` for it when the tailnet
    /// allows. Run it again to repair an install
    Install {
        /// Loopback port to listen on, saved as `[serve] port`
        #[arg(long)]
        port: Option<u16>,
        /// Leave `tailscale serve` as it is
        #[arg(long)]
        no_tailscale: bool,
        /// Also pair a device with this name, printing its QR code
        #[arg(long, value_name = "DEVICE")]
        pair: Option<String>,
    },
    /// Remove the LaunchAgent `install` wrote
    Uninstall,
    /// Whether the server runs, and what reaches it
    Status,
    /// Print the end of the server's log
    Logs {
        /// How many lines
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// Keep printing what is appended
        #[arg(short, long)]
        follow: bool,
    },
}
