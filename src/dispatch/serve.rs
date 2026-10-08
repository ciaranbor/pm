use std::time::Duration;

use crate::cli::ServeCommands;
use pm::commands::serve;
use pm::commands::serve_pair::{self, Pairing};
use pm::commands::{serve_install, serve_logs, serve_status};
use pm::error::Result;
use pm::state::paths;
use pm::state::project::GlobalConfig;
use pm::state::serve_files::ServeFiles;
use pm::tailscale::{self, Serving};

/// How long `tailscale serve --bg` may take before install gives up on it.
const TAILSCALE_SERVE_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) fn dispatch_serve(
    command: Option<ServeCommands>,
    port: Option<u16>,
    server: Option<&str>,
) -> Result<()> {
    let config_dir = paths::global_config_dir()?;
    let files = ServeFiles::global()?;
    match command {
        None => {
            let mut config = serve::Config::new(paths::global_projects_dir()?, files, server);
            let hosts = GlobalConfig::load(&config_dir)?.serve.push_hosts;
            config.push = serve::PushPolicy::new(&hosts);
            let port = port.unwrap_or_else(|| serve::configured_port(&config_dir));
            serve::serve(config, port)
        }
        Some(ServeCommands::Pair { name, url }) => {
            print_pairing(&serve_pair::pair(&files, &name, url.as_deref())?)
        }
        Some(ServeCommands::Devices) => {
            let lines = pm::commands::serve_devices::devices(&files.devices())?;
            if lines.is_empty() {
                println!("No devices paired; `pm serve pair` pairs one.");
            }
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        Some(ServeCommands::Revoke { device }) => {
            pm::commands::serve_revoke::revoke(&files, &device)?;
            println!("Revoked {device}.");
            Ok(())
        }
        Some(ServeCommands::Install {
            port,
            no_tailscale,
            pair,
        }) => install(&config_dir, &files, server, port, no_tailscale, pair),
        Some(ServeCommands::Uninstall) => {
            if serve_install::uninstall()? {
                println!("Uninstalled the pm serve LaunchAgent.");
            } else {
                println!("No pm serve LaunchAgent was installed.");
            }
            println!(
                "`tailscale serve` is left as it is; `tailscale serve --https=443 off` stops it."
            );
            Ok(())
        }
        Some(ServeCommands::Status) => {
            let port = serve::configured_port(&config_dir);
            let exe = std::env::current_exe()?;
            for line in serve_status::status(&paths::home_dir()?, &files, port, &exe) {
                println!("{line}");
            }
            Ok(())
        }
        Some(ServeCommands::Logs { lines, follow }) => {
            serve_logs::logs(&files.log(), lines, follow, &mut std::io::stdout())
        }
    }
}

fn print_pairing(pairing: &Pairing) -> Result<()> {
    println!("{}", pairing.qr()?);
    println!("url:    {}", pairing.url);
    println!("device: {}", pairing.device);
    println!("token:  {}", pairing.token);
    println!(
        "The token is shown only now; `pm serve revoke {}` withdraws it.",
        pairing.device
    );
    Ok(())
}

fn install(
    config_dir: &std::path::Path,
    files: &ServeFiles,
    server: Option<&str>,
    port: Option<u16>,
    no_tailscale: bool,
    pair: Option<String>,
) -> Result<()> {
    serve_install::macos_only()?;
    if let Some(port) = port {
        let mut config = GlobalConfig::load(config_dir)?;
        config.serve.port = Some(port);
        config.save(config_dir)?;
    }
    let port = serve::configured_port(config_dir);
    let installed = serve_install::install(server)?;
    println!("Installed {}.", installed.plist.display());
    match installed.started {
        Ok(pid) => println!("pm serve runs under launchd (pid {pid}) on 127.0.0.1:{port}."),
        Err(Some(holder)) => println!(
            "Another pm serve (pid {holder}) runs outside launchd; launchd's starts once it exits."
        ),
        Err(None) => println!("pm serve has not started yet; see `pm serve logs`."),
    }

    let mut serving = tailscale::check(port);
    if serving == Serving::NotServing && !no_tailscale {
        match tailscale::serve(port, TAILSCALE_SERVE_TIMEOUT) {
            Ok(()) => serving = tailscale::check(port),
            Err(e) => println!("`tailscale serve --bg {port}` failed: {e}"),
        }
    }
    println!("{}", serving.advice(port));

    if let Some(device) = pair {
        match serve_pair::pair(files, &device, None) {
            Ok(pairing) => print_pairing(&pairing)?,
            Err(e) => println!(
                "Not paired: {e}. `pm serve pair --name {device} --url <url>` pairs it with the URL the phone reaches."
            ),
        }
    }
    Ok(())
}
