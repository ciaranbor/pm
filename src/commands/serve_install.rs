//! `pm serve install` and `uninstall`: run `pm serve` as a launchd
//! LaunchAgent, started at login and restarted if it exits, so it survives
//! reboots of an always-on Mac. launchd gives its jobs a bare `PATH`, so
//! the agent carries the installing shell's, where `tmux` and `git` are
//! found; `PM_TMUX_SERVER`, when set, goes with it. Its output goes to
//! `serve.log` beside the devices file.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::launchd;
use crate::state::devices;
use crate::state::paths;

const LABEL: &str = "dev.pm.serve";

fn plist_path() -> Result<PathBuf> {
    Ok(paths::home_dir()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn macos_only() -> Result<()> {
    if cfg!(target_os = "macos") {
        return Ok(());
    }
    Err(PmError::Serve(
        "install uses launchd, which is macOS only; run `pm serve` under your service manager"
            .into(),
    ))
}

/// Write the LaunchAgent serving on `port` and (re)load it. Returns the
/// plist's path and the log's.
pub fn install(port: u16, tmux_server: Option<&str>) -> Result<(PathBuf, PathBuf)> {
    macos_only()?;
    let exe = std::env::current_exe()?;
    let log = paths::global_config_dir()?
        .join(devices::DIR_NAME)
        .join("serve.log");
    std::fs::create_dir_all(log.parent().expect("a file in a dir"))?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    let plist = plist_path()?;
    write_atomic(
        &plist,
        render(&exe, port, &path_env, tmux_server, &log).as_bytes(),
    )?;
    launchd::bootout(LABEL)?;
    launchd::bootstrap(&plist)?;
    Ok((plist, log))
}

/// Unload the LaunchAgent and remove its plist. Whether one was installed.
pub fn uninstall() -> Result<bool> {
    macos_only()?;
    launchd::bootout(LABEL)?;
    let plist = plist_path()?;
    match std::fs::remove_file(&plist) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn render(exe: &Path, port: u16, path_env: &str, tmux_server: Option<&str>, log: &Path) -> String {
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
  <array>{exe}<string>serve</string><string>--port</string><string>{port}</string></array>
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
        let plist = render(
            Path::new("/opt/a&b/pm"),
            8123,
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
            serde_json::json!(["/opt/a&b/pm", "serve", "--port", "8123"])
        );
        assert_eq!(
            parsed["EnvironmentVariables"],
            serde_json::json!({ "PATH": "/opt/<brew>/bin:/usr/bin", "PM_TMUX_SERVER": "work" })
        );
        assert_eq!(parsed["KeepAlive"], true);
    }
}
