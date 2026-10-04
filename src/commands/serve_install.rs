//! `pm serve install` and `uninstall`: run `pm serve` as a launchd
//! LaunchAgent, started at login and restarted if it exits, so it survives
//! reboots of an always-on Mac. launchd gives its jobs a bare `PATH`, so
//! the agent carries the installing shell's, where `tmux` and `git` are
//! found; `PM_TMUX_SERVER`, when set, goes with it. It gets a UTF-8 `LANG`
//! too (the installing shell's, if UTF-8), which launchd leaves unset. Its
//! output goes to `serve.log` beside the devices file. The port is
//! `[serve] port`, read as the server starts, so the plist names none.
//! Installing again repairs an install; `pm upgrade` refreshes one
//! ([`refresh`]).

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
    let lang = utf8_lang(std::env::var("LANG").ok());
    let plist = plist_path(&paths::home_dir()?);
    write_atomic(
        &plist,
        render(&exe, &path_env, &lang, tmux_server, &log).as_bytes(),
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

/// What [`refresh`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Refresh {
    NotInstalled,
    Current,
    /// The plist was out of date: rewritten and reloaded, or with
    /// `dry_run`, left as it was.
    Updated,
}

/// Bring the installed agent's plist under `home` up to this pm's template,
/// keeping its executable and the `PATH`, `LANG` and `PM_TMUX_SERVER` its
/// install recorded (an older plist with no `LANG` gets the current
/// shell's, if UTF-8). `reload` runs only when the file changed.
pub fn refresh(
    home: &Path,
    config_dir: &Path,
    dry_run: bool,
    reload: impl FnOnce(&Path) -> Result<()>,
) -> Result<Refresh> {
    let plist = plist_path(home);
    let old = match std::fs::read_to_string(&plist) {
        Ok(old) => old,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Refresh::NotInstalled),
        Err(e) => return Err(e.into()),
    };
    let exe = plist_exe(&old).ok_or_else(|| {
        PmError::Serve(format!(
            "{} names no executable; `pm serve install` rewrites it",
            plist.display()
        ))
    })?;
    let path_env = plist_env(&old, "PATH")
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let lang = utf8_lang(plist_env(&old, "LANG").or_else(|| std::env::var("LANG").ok()));
    let tmux_server = plist_env(&old, "PM_TMUX_SERVER");
    let new = render(
        &exe,
        &path_env,
        &lang,
        tmux_server.as_deref(),
        &state::log_path(config_dir),
    );
    if new == old {
        return Ok(Refresh::Current);
    }
    if !dry_run {
        write_atomic(&plist, new.as_bytes())?;
        reload(&plist)
            .map_err(|e| PmError::Serve(format!("{e}; `pm serve install` reloads the agent")))?;
    }
    Ok(Refresh::Updated)
}

/// Unload the agent and load it again from `plist`.
pub fn reload(plist: &Path) -> Result<()> {
    launchd::bootout(LABEL)?;
    launchd::bootstrap(plist)
}

/// `lang` when it names UTF-8, else a UTF-8 locale every Mac has.
fn utf8_lang(lang: Option<String>) -> String {
    lang.filter(|l| {
        let l = l.to_ascii_lowercase();
        l.contains("utf-8") || l.contains("utf8")
    })
    .unwrap_or_else(|| "en_US.UTF-8".into())
}

fn render(exe: &Path, path_env: &str, lang: &str, tmux_server: Option<&str>, log: &Path) -> String {
    let string = |s: &str| format!("<string>{}</string>", escape(s));
    let mut env = format!(
        "<key>PATH</key>{}<key>LANG</key>{}",
        string(path_env),
        string(lang)
    );
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
    Some(PathBuf::from(unescape(exe)))
}

/// The value of `key` in the plist's `EnvironmentVariables`.
fn plist_env(plist: &str, key: &str) -> Option<String> {
    let env = plist.split("<key>EnvironmentVariables</key>").nth(1)?;
    let env = env.split("</dict>").next()?;
    let value = env
        .split(&format!("<key>{key}</key>"))
        .nth(1)?
        .trim_start()
        .strip_prefix("<string>")?
        .split("</string>")
        .next()?;
    Some(unescape(value))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod lang_tests {
    use super::utf8_lang;

    #[test]
    fn the_agent_keeps_a_utf8_lang_and_replaces_any_other() {
        assert_eq!(utf8_lang(Some("de_DE.utf8".into())), "de_DE.utf8");
        assert_eq!(utf8_lang(Some("C".into())), "en_US.UTF-8");
        assert_eq!(utf8_lang(None), "en_US.UTF-8");
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use std::cell::Cell;

    fn must_not_reload(_: &Path) -> Result<()> {
        panic!("reloaded the agent")
    }

    #[test]
    fn an_absent_agent_is_left_uninstalled() {
        let home = tempfile::tempdir().unwrap();
        let refreshed = refresh(home.path(), &home.path().join("pm"), false, must_not_reload);
        assert_eq!(refreshed.unwrap(), Refresh::NotInstalled);
        assert!(!plist_path(home.path()).exists());
    }

    #[test]
    fn a_stale_plist_is_rewritten_keeping_its_install_choices_and_reloaded_once() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join("pm");
        let plist = plist_path(home.path());
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        // What an install before `LANG` was added wrote.
        let stale = render(
            Path::new("/opt/a&b/pm"),
            "/opt/<brew>/bin:/usr/bin",
            "en_US.UTF-8",
            Some("work"),
            &state::log_path(&config_dir),
        )
        .replace("<key>LANG</key><string>en_US.UTF-8</string>", "");
        std::fs::write(&plist, &stale).unwrap();

        let dry = refresh(home.path(), &config_dir, true, must_not_reload).unwrap();
        assert_eq!(dry, Refresh::Updated);
        assert_eq!(std::fs::read_to_string(&plist).unwrap(), stale);

        let reloads = Cell::new(0);
        let refreshed = refresh(home.path(), &config_dir, false, |p| {
            assert_eq!(p, plist);
            reloads.set(reloads.get() + 1);
            Ok(())
        });
        assert_eq!(refreshed.unwrap(), Refresh::Updated);
        assert_eq!(reloads.get(), 1);
        assert_eq!(
            std::fs::read_to_string(&plist).unwrap(),
            render(
                Path::new("/opt/a&b/pm"),
                "/opt/<brew>/bin:/usr/bin",
                &utf8_lang(std::env::var("LANG").ok()),
                Some("work"),
                &state::log_path(&config_dir),
            )
        );

        let again = refresh(home.path(), &config_dir, false, must_not_reload).unwrap();
        assert_eq!(again, Refresh::Current);
    }

    #[test]
    fn a_plist_naming_no_executable_is_an_error_and_left_as_it_is() {
        let home = tempfile::tempdir().unwrap();
        let plist = plist_path(home.path());
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        let mangled = "<plist><dict><key>Label</key><string>dev.pm.serve</string></dict></plist>";
        std::fs::write(&plist, mangled).unwrap();

        assert!(refresh(home.path(), &home.path().join("pm"), false, must_not_reload).is_err());
        assert_eq!(std::fs::read_to_string(&plist).unwrap(), mangled);
    }
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
            "en_IE.UTF-8",
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
            serde_json::json!({
                "PATH": "/opt/<brew>/bin:/usr/bin",
                "LANG": "en_IE.UTF-8",
                "PM_TMUX_SERVER": "work"
            })
        );
        assert_eq!(parsed["StandardErrorPath"], "/logs/serve.log");
        assert_eq!(parsed["KeepAlive"], true);
        assert_eq!(plist_exe(&plist).as_deref(), Some(exe));
    }
}
