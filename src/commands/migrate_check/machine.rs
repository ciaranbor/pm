//! What lives only on this machine: nothing pm syncs carries it, so each
//! item is a manual step on the new host. Secrets are named, never read.

use std::path::Path;

use crate::error::Result;
use crate::git;
use crate::harness::{Harness, Probe};
use crate::state::devices::Devices;
use crate::state::project::HarnessConfig;
use crate::state::serve_files::ServeFiles;

use super::super::skills::{CANONICAL_DIR, GlobalStore, global_customs_in};
use super::{Finding, THIS_HOST, shell_path};

/// The marker of a tmux config that loads pm's plugin.
const TMUX_INIT: &str = "pm tmux init";

/// Where tmux reads its config from, under `home`.
fn tmux_configs(home: &Path) -> [std::path::PathBuf; 2] {
    [home.join(".tmux.conf"), home.join(".config/tmux/tmux.conf")]
}

pub(super) struct Machine<'a> {
    pub home: &'a Path,
    pub config_dir: &'a Path,
    /// The harnesses agents run on, with the settings in effect for each.
    pub harnesses: &'a [(Harness, HarnessConfig)],
    /// The projects checked, as `--project` names them; empty for all.
    pub projects: &'a [String],
    pub probe: Probe,
}

/// The manual steps, in the order they are done: exports on this host,
/// then installs, logins, the registry pull and restore on the new host,
/// then the machine-local files to carry over.
pub(super) fn findings(m: &Machine<'_>) -> Result<Vec<Finding>> {
    let tarball = |h: &Harness| format!("pm-{h}.tar.gz");
    let selected: String = m
        .projects
        .iter()
        .map(|project| format!(" --project {project}"))
        .collect();
    let mut exported: Vec<Harness> = Vec::new();
    for (harness, _) in m.harnesses {
        if !exported.contains(harness) {
            exported.push(*harness);
        }
    }
    let mut out: Vec<Finding> = exported
        .iter()
        .map(|h| {
            Finding::manual(
                THIS_HOST,
                format!(
                    "pm harness export --all{selected} --harness {h} -o {}",
                    tarball(h)
                ),
                Some(format!(
                    "{h} conversations travel only in an export; run it once agents are stopped"
                )),
            )
        })
        .collect();

    let version = crate::version::VERSION;
    let script = format!(
        "curl -fsSL {} | PM_VERSION={} sh",
        crate::commands::self_update::INSTALL_SCRIPT,
        crate::version::base(version)
    );
    out.push(Finding::manual(
        "",
        if crate::version::is_dev(version) {
            format!(
                "install pm {version}: `cargo install --path .` at the commit it was built from, or its release: `{script}`"
            )
        } else {
            format!("install pm {version}: `{script}`")
        },
        None,
    ));

    // One line per distinct answer: projects with different settings for a
    // harness (an opencode binary, provider keys) can each need their own.
    let mut lines: Vec<String> = Vec::new();
    for (harness, config) in m.harnesses {
        let needs = harness
            .min_version()
            .map(|min| format!("; pm needs {min} or later"))
            .unwrap_or_default();
        let version = match harness.installed_version(config, m.probe) {
            Some(version) => format!("install {harness} (`{version}` here{needs})"),
            None => format!("install {harness} (agents use it, but it can't be run here{needs})"),
        };
        for line in [version, harness.credentials_step(config)] {
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    out.extend(lines.into_iter().map(|l| Finding::manual("", l, None)));

    let registry = if git::is_git_repo(m.config_dir) {
        git::remote_url(m.config_dir, "origin")?
    } else {
        None
    };
    out.push(Finding::manual(
        "",
        format!(
            "pm state init --global --remote {}",
            registry.as_deref().unwrap_or(super::plan::REGISTRY_URL)
        ),
        Some("pulls the registry, which names every project to restore".to_string()),
    ));
    let mut restore = format!("pm restore{selected}");
    for harness in &exported {
        restore.push_str(&format!(" --import {}", tarball(harness)));
    }
    out.push(Finding::manual(
        "",
        restore,
        Some(
            "clones each project, pulls its state, recreates its worktrees, imports the \
             conversations, then starts agents"
                .to_string(),
        ),
    ));

    let serve = ServeFiles::in_dirs(&crate::state::paths::dirs_under(m.home));
    if let Ok(devices) = Devices::load(&serve.devices())
        && !devices.devices.is_empty()
    {
        let names: Vec<&str> = devices.devices.keys().map(String::as_str).collect();
        out.push(Finding::manual(
            "",
            format!(
                "`pm serve pair <name>` for each device: {}",
                names.join(", ")
            ),
            Some(format!(
                "paired devices and the push key stay here; or carry {} over by hand",
                shell_path(&serve.dir)
            )),
        ));
    }
    if super::super::serve_install::plist_path(m.home).exists() {
        out.push(Finding::manual(
            "",
            "pm serve install".to_string(),
            Some("`pm serve` runs under launchd here".to_string()),
        ));
    }

    for config in tmux_configs(m.home) {
        if std::fs::read_to_string(&config).is_ok_and(|text| text.contains(TMUX_INIT)) {
            out.push(Finding::manual(
                "",
                format!("copy {} (loads pm's tmux plugin)", shell_path(&config)),
                None,
            ));
        }
    }

    let customs = global_customs_in(&GlobalStore {
        home: m.home.to_path_buf(),
        config_dir: m.config_dir.to_path_buf(),
    });
    if !customs.is_empty() {
        out.push(Finding::manual(
            "",
            format!(
                "copy your own skills and agent definitions from {}: {}",
                shell_path(&m.home.join(CANONICAL_DIR)),
                customs.join(", ")
            ),
            Some("only bundled ones are installed on the new host".to_string()),
        ));
    }

    let secrets = m.home.join(".config/pm-secrets");
    if secrets.is_dir() {
        out.push(Finding::manual(
            "",
            format!("copy {} over a secure channel", shell_path(&secrets)),
            Some("it is machine-local".to_string()),
        ));
    }
    Ok(out)
}
