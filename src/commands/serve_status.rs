//! `pm serve status`, and what `pm doctor` warns of: whether the server
//! runs, whether its LaunchAgent is current, and whether paired devices can
//! reach it.
//!
//! Doctor's checks read files only — the plist, the lock, `state.json`, the
//! devices — plus, at full depth, the local `tailscale` CLI; never
//! `launchctl`, so they hold under a test home.

use std::path::{Path, PathBuf};

use crate::state::devices::Devices;
use crate::state::serve_files::ServeFiles;
use crate::tailscale::Serving;

use super::serve::state::{self, State};
use super::serve_install::{LABEL, plist_dirs, plist_exe, plist_path};
use crate::state::dirs::Dirs;
use crate::state::paths;

/// The server as its files show it.
pub struct Facts {
    /// The running server's state; `None` when none runs.
    pub running: Option<State>,
    /// The installed agent's executable, when its plist exists.
    pub installed: Option<PathBuf>,
    /// Where the installed agent's server keeps pm's files, by the XDG
    /// variables its plist records, and where this process does.
    pub dirs: Option<(Dirs, Dirs)>,
    pub devices: usize,
    pub port: u16,
}

impl Facts {
    pub fn read(home: &Path, files: &ServeFiles, port: u16) -> Self {
        let running = state::held(files)
            .unwrap_or(false)
            .then(|| State::load(files))
            .flatten();
        let plist = std::fs::read_to_string(plist_path(home)).ok();
        Self {
            running,
            installed: plist
                .as_ref()
                .map(|plist| plist_exe(plist).unwrap_or_default()),
            dirs: plist.map(|plist| (plist_dirs(home, &plist), paths::dirs_under(home))),
            devices: Devices::load(&files.devices())
                .map(|d| d.devices.len())
                .unwrap_or(0),
            port,
        }
    }

    /// Doctor's warnings for this pm, at `exe` and of `version`.
    /// `tailscale` says what `tailscale serve` does for the port, asked only
    /// while a device is paired.
    pub fn warnings(
        &self,
        exe: &Path,
        version: &str,
        tailscale: impl FnOnce(u16) -> Option<Serving>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut warn = |line: String| warnings.push(format!("serve — {line}"));
        if let Some(installed) = &self.installed {
            if self.running.is_none() {
                warn(
                    "the LaunchAgent is installed but `pm serve` isn't running; see `pm serve logs`, and `pm serve install` reinstalls it".into(),
                );
            }
            if installed != exe {
                warn(format!(
                    "the LaunchAgent runs {}, not this pm ({}); run `pm serve install`",
                    installed.display(),
                    exe.display()
                ));
            }
        }
        if let Some((agent, here)) = &self.dirs
            && (agent.config != here.config || agent.state != here.state)
        {
            warn(format!(
                "the LaunchAgent's server keeps pm's files in {} and {}, this shell in {} and {}, \
                 so they see different registries; set the XDG variables alike and run `pm serve install`",
                agent.config.display(),
                agent.state.display(),
                here.config.display(),
                here.state.display()
            ));
        }
        if let Some(running) = &self.running
            && running.version != version
        {
            let theirs = if running.version.is_empty() {
                "an older pm".to_string()
            } else {
                format!("pm {}", running.version)
            };
            warn(format!(
                "the running server is {theirs}, not this pm ({version}); restart it with `pm serve install`"
            ));
        }
        if self.devices > 0 {
            if self.installed.is_none() && self.running.is_none() {
                warn(format!(
                    "{} but `pm serve` isn't running; run `pm serve install`",
                    devices(self.devices)
                ));
            }
            match tailscale(self.port) {
                None | Some(Serving::NotInstalled | Serving::Serving(_)) => {}
                Some(serving) => warn(serving.advice(self.port)),
            }
        }
        warnings
    }
}

/// `pm serve status`: one line per aspect, then doctor's warnings but
/// Tailscale's, which has a line of its own.
pub fn status(home: &Path, files: &ServeFiles, port: u16, exe: &Path) -> Vec<String> {
    let facts = Facts::read(home, files, port);
    let mut lines = Vec::new();
    lines.push(match &facts.running {
        Some(s) => format!(
            "server:    listening on 127.0.0.1:{} (pid {}, pm {}) since {}",
            s.port,
            s.pid,
            if s.version.is_empty() {
                "?"
            } else {
                &s.version
            },
            s.started
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
        ),
        None => "server:    not running".into(),
    });
    if cfg!(target_os = "macos") {
        let agent = match (&facts.installed, crate::launchd::job(LABEL)) {
            (None, _) => "not installed (`pm serve install`)".into(),
            (Some(_), Ok(Some(Some(pid)))) => format!("loaded, pid {pid}"),
            (Some(_), Ok(Some(None))) => "loaded, not running".into(),
            (Some(_), Ok(None)) => "installed, not loaded".into(),
            (Some(_), Err(e)) => format!("installed; {e}"),
        };
        lines.push(format!("launchd:   {LABEL} {agent}"));
    }
    lines.push(format!("devices:   {}", devices(facts.devices)));
    lines.push(format!(
        "tailscale: {}",
        crate::tailscale::check(port).advice(port)
    ));
    lines.push(format!("log:       {}", files.log().display()));
    lines.extend(
        facts
            .warnings(exe, crate::version::VERSION, |_| None)
            .into_iter()
            .map(|w| format!("warning:   {}", w.trim_start_matches("serve — "))),
    );
    lines
}

pub fn devices(n: usize) -> String {
    format!("{n} device{} paired", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_warns_of_an_installed_server_not_running_or_running_another_pm_or_version() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let config = ServeFiles::new(dir.path().join("config"), dir.path().join("config"));
        let exe = Path::new("/usr/local/bin/pm");
        std::fs::create_dir_all(home.join("Library/LaunchAgents")).unwrap();
        let version = crate::version::VERSION;
        let warnings =
            |facts: &Facts| facts.warnings(exe, version, |_| panic!("no device is paired"));
        assert!(warnings(&Facts::read(&home, &config, 7764)).is_empty());

        let plist = format!(
            "<key>ProgramArguments</key><array><string>{}</string></array>",
            exe.display()
        );
        std::fs::write(plist_path(&home), &plist).unwrap();
        State {
            pid: 999_999,
            ..State::now(7764)
        }
        .save(&config)
        .unwrap();
        let found = warnings(&Facts::read(&home, &config, 7764));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("isn't running"),
            "a state.json without the lock held is a dead server: {found:?}"
        );

        let _running = state::lock(&config).unwrap().unwrap();
        std::fs::write(plist_path(&home), plist.replace("/usr/local", "/opt/old")).unwrap();
        let found = warnings(&Facts::read(&home, &config, 7764));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("/opt/old/bin/pm"), "{found:?}");

        let elsewhere = plist.replace(
            "</array>",
            "</array><key>EnvironmentVariables</key><dict><key>XDG_STATE_HOME</key>\
             <string>/elsewhere</string></dict>",
        );
        std::fs::write(plist_path(&home), elsewhere).unwrap();
        let found = warnings(&Facts::read(&home, &config, 7764));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("/elsewhere/pm"), "{found:?}");

        std::fs::write(plist_path(&home), &plist).unwrap();
        State {
            version: "0.0.1".into(),
            ..State::now(7764)
        }
        .save(&config)
        .unwrap();
        let found = warnings(&Facts::read(&home, &config, 7764));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("pm 0.0.1, not this pm"), "{found:?}");
    }
}
