//! `pm serve install` and `uninstall`: run `pm serve` as a launchd
//! LaunchAgent, started at login and restarted if it exits, so it survives
//! reboots of an always-on Mac. launchd gives its jobs a bare `PATH`, so
//! the agent carries the installing shell's, where `tmux` and `git` are
//! found; `PM_TMUX_SERVER`, when set, goes with it. Its output goes to
//! `serve.log` beside the devices file. The port is `[serve] port`, read as
//! the server starts, so the plist names none. Installing again repairs an
//! install.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::{PmError, Result};
use crate::fs_utils::{process_alive, write_atomic};
use crate::launchd;
use crate::state::paths;

use super::serve::state::{self, State};

pub const LABEL: &str = "dev.pm.serve";

/// How long install waits for the agent's server to start.
const WAIT: Duration = Duration::from_secs(5);

/// The agent's plist under `home`.
pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

pub fn macos_only() -> Result<()> {
    if cfg!(target_os = "macos") {
        return Ok(());
    }
    Err(PmError::Serve(
        "install uses launchd, which is macOS only; run `pm serve` under your service manager"
            .into(),
    ))
}

pub struct Installed {
    pub plist: PathBuf,
    /// The agent's server, once it started; else what held the lock, if
    /// anything did.
    pub started: std::result::Result<u32, Option<u32>>,
}

/// Write the LaunchAgent and (re)load it.
pub fn install(tmux_server: Option<&str>) -> Result<Installed> {
    macos_only()?;
    let config_dir = paths::global_config_dir()?;
    let exe = std::env::current_exe()?;
    let log = state::log_path(&config_dir);
    std::fs::create_dir_all(log.parent().expect("a file in a dir"))?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    let plist = plist_path(&paths::home_dir()?);
    write_atomic(
        &plist,
        render(&exe, &path_env, tmux_server, &log).as_bytes(),
    )?;
    launchd::bootout(LABEL)?;
    let began = chrono::Utc::now();
    launchd::bootstrap(&plist)?;

    let start = Instant::now();
    let started = loop {
        let state = State::load(&config_dir);
        if let Some(state) = state.as_ref().filter(|s| s.started >= began) {
            break Ok(state.pid);
        }
        if start.elapsed() > WAIT {
            let holder = state
                .filter(|s| process_alive(s.pid) && state::held(&config_dir).unwrap_or(false))
                .map(|s| s.pid);
            break Err(holder);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    Ok(Installed { plist, started })
}

/// Unload the LaunchAgent and remove its plist. Whether one was installed.
pub fn uninstall() -> Result<bool> {
    macos_only()?;
    launchd::bootout(LABEL)?;
    match std::fs::remove_file(plist_path(&paths::home_dir()?)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn render(exe: &Path, path_env: &str, tmux_server: Option<&str>, log: &Path) -> String {
    let string = |s: &str| format!("<string>{}</string>", escape(s));
    let mut env = format!("<key>PATH</key>{}", string(path_env));
    if let Some(server) = tmux_server {
        env.push_str(&format!("<key>PM_TMUX_SERVER</key>{}", string(server)));
    }
    let log = log.to_string_lossy();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>{label}
  <key>ProgramArguments</key>
  <array>{exe}<string>serve</string></array>
  <key>EnvironmentVariables</key>
  <dict>{env}</dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key>{log}
  <key>StandardErrorPath</key>{log}
</dict>
</plist>
"#,
        label = string(LABEL),
        exe = string(&exe.to_string_lossy()),
        log = string(&log),
    )
}

/// The executable a plist install wrote runs.
pub fn plist_exe(plist: &str) -> Option<PathBuf> {
    let args = plist.split("<key>ProgramArguments</key>").nth(1)?;
    let exe = args.split("<string>").nth(1)?.split("</string>").next()?;
    Some(PathBuf::from(
        exe.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    ))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[test]
    fn the_plist_launchd_reads_runs_pm_serve_with_the_installing_shells_path() {
        let exe = Path::new("/opt/a&b/pm");
        let plist = render(
            exe,
            "/opt/<brew>/bin:/usr/bin",
            Some("work"),
            Path::new("/logs/serve.log"),
        );
        let mut plutil = Command::new("plutil")
            .args(["-convert", "json", "-o", "-", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        plutil
            .stdin
            .take()
            .unwrap()
            .write_all(plist.as_bytes())
            .unwrap();
        let out = plutil.wait_with_output().unwrap();
        assert!(out.status.success(), "{plist}");
        let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

        assert_eq!(
            parsed["ProgramArguments"],
            serde_json::json!(["/opt/a&b/pm", "serve"])
        );
        assert_eq!(
            parsed["EnvironmentVariables"],
            serde_json::json!({ "PATH": "/opt/<brew>/bin:/usr/bin", "PM_TMUX_SERVER": "work" })
        );
        assert_eq!(parsed["StandardErrorPath"], "/logs/serve.log");
        assert_eq!(parsed["KeepAlive"], true);
        assert_eq!(plist_exe(&plist).as_deref(), Some(exe));
    }
}
