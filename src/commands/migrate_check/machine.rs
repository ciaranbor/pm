//! What lives only on this machine: nothing pm syncs carries it, so each
//! item is a manual step on the new host. Secrets are named, never read.

use std::path::Path;

use crate::harness::{Harness, Probe};
use crate::state::devices::Devices;
use crate::state::project::HarnessConfig;

use super::super::skills::{CANONICAL_DIR, GlobalStore, global_customs_in};
use super::{Finding, shell_path};

/// The marker of a tmux config that loads pm's plugin.
const TMUX_INIT: &str = "pm tmux init";

/// Where tmux reads its config from, under `home`.
fn tmux_configs(home: &Path) -> [std::path::PathBuf; 2] {
    [home.join(".tmux.conf"), home.join(".config/tmux/tmux.conf")]
}

pub(super) fn findings(
    home: &Path,
    config_dir: &Path,
    harnesses: &[(Harness, HarnessConfig)],
    probe: Probe,
) -> Vec<Finding> {
    let mut out = vec![Finding::manual(format!(
        "pm {} runs here: on the new host, `cargo install --path .` in a checkout of the \
         commit it was built from",
        env!("CARGO_PKG_VERSION")
    ))];

    // One line per distinct answer: projects with different settings for a
    // harness (an opencode binary, provider keys) can each need their own.
    let mut lines: Vec<String> = Vec::new();
    for (harness, config) in harnesses {
        let needs = harness
            .min_version()
            .map(|min| format!("; pm needs {min} or later"))
            .unwrap_or_default();
        let version = match harness.installed_version(config, probe) {
            Some(version) => {
                format!("{harness}: `{version}` here — install it on the new host{needs}")
            }
            None => format!(
                "{harness}: agents use it but it can't be run here — install it on the new \
                 host{needs}"
            ),
        };
        let credentials = format!(
            "{harness} credentials don't travel: {}",
            harness.credentials_step(config)
        );
        for line in [version, credentials] {
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    out.extend(lines.into_iter().map(Finding::manual));

    if let Ok(devices) = Devices::load(&Devices::path(config_dir))
        && !devices.devices.is_empty()
    {
        let names: Vec<&str> = devices.devices.keys().map(String::as_str).collect();
        out.push(Finding::manual(format!(
            "`pm serve` paired devices ({}) and its push key stay here: pair each again on the \
             new host (`pm serve pair <name>`), or carry {} over by hand",
            names.join(", "),
            shell_path(&config_dir.join(crate::state::devices::DIR_NAME))
        )));
    }
    if super::super::serve_install::plist_path(home).exists() {
        out.push(Finding::manual(
            "`pm serve` runs under launchd here: `pm serve install` on the new host".to_string(),
        ));
    }

    for config in tmux_configs(home) {
        if std::fs::read_to_string(&config).is_ok_and(|text| text.contains(TMUX_INIT)) {
            out.push(Finding::manual(format!(
                "{} loads pm's tmux plugin (`{TMUX_INIT}`): carry it to the new host",
                shell_path(&config)
            )));
        }
    }

    let customs = global_customs_in(&GlobalStore {
        home: home.to_path_buf(),
        config_dir: config_dir.to_path_buf(),
    });
    if !customs.is_empty() {
        out.push(Finding::manual(format!(
            "your own global skills and agent definitions live only in {}: carry {} by hand",
            shell_path(&home.join(CANONICAL_DIR)),
            customs.join(", ")
        )));
    }

    let secrets = home.join(".config/pm-secrets");
    if secrets.is_dir() {
        out.push(Finding::manual(format!(
            "{} is machine-local: copy it over a secure channel yourself",
            shell_path(&secrets)
        )));
    }
    out
}
