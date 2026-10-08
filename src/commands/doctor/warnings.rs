//! Project-independent warnings: the global config, the registry,
//! `pm serve`, the macOS login keychain, and whether each harness can
//! deliver the shared baseline.

use std::path::Path;

use super::Depth;
use crate::commands::skills;
use crate::error::Result;
use crate::harness::{Harness, Probe};
use crate::state::paths;
use crate::state::project::{GlobalConfig, Presence, ProjectEntry, harness_config};

/// Warn when the global `config.toml` exists but doesn't parse. Every reader
/// falls back to defaults on a parse error so that a bad file can never block a
/// spawn, which leaves `pm doctor` as the only place a user hand-editing it can
/// learn that `max_features` and all their per-agent models and permission
/// modes are being ignored.
pub(super) fn global_config_warning() -> Option<String> {
    global_config_warning_in(&paths::global_config_dir().ok()?)
}

fn global_config_warning_in(config_dir: &Path) -> Option<String> {
    let err = GlobalConfig::load(config_dir).err()?;
    let path = config_dir.join("config.toml").display().to_string();
    Some(format!(
        "global config — {path} could not be read ({err}); max_features, [agents.models] and [agents.permissions] from it are all being ignored"
    ))
}

/// `pm serve` ([`serve_status`](crate::commands::serve_status)). Tailscale
/// is asked only at full depth.
pub(super) fn serve_warnings(depth: Depth) -> Vec<String> {
    let (Ok(home), Ok(config_dir), Ok(exe)) = (
        paths::home_dir(),
        paths::global_config_dir(),
        std::env::current_exe(),
    ) else {
        return Vec::new();
    };
    let port = crate::commands::serve::configured_port(&config_dir);
    crate::commands::serve_status::Facts::read(&home, &config_dir, port).warnings(
        &exe,
        crate::version::VERSION,
        |port| (depth == Depth::Full).then(|| crate::tailscale::check(port)),
    )
}

/// The macOS login keychain ([`keychain`](crate::keychain)), asked only at
/// full depth: some harnesses read it as they start
/// ([`Harness::reads_keychain`]).
pub(super) fn keychain_warning(depth: Depth) -> Option<String> {
    use crate::keychain::{self, Answer};
    if depth != Depth::Full {
        return None;
    }
    let command = keychain::COMMAND;
    let readers = keychain::readers(Harness::SUPPORTED.iter().copied());
    match keychain::check()? {
        Answer::Answered => None,
        Answer::Hung => Some(format!(
            "keychain — the macOS login keychain did not answer `{command}` within {}s; \
             {readers} read it as they start, so an agent of theirs launched now waits on it \
             and does not come up",
            keychain::LIMIT.as_secs()
        )),
        Answer::Error(said) => Some(format!(
            "keychain — note: the macOS login keychain answered `{command}` with an error \
             ({said}), as a locked one does over ssh; {readers} read it as they start, so an \
             agent of theirs launched from here may not come up"
        )),
    }
}

/// Warn about each registry entry that can't be read, which all-project
/// commands skip, and each root that holds no main checkout. A root that is
/// missing altogether is not warned about: a registry synced between
/// machines names projects a machine never uses.
pub(super) fn registry_warnings(projects_dir: &Path) -> Result<Vec<String>> {
    let registry = ProjectEntry::scan(projects_dir)?;
    let unreadable = registry.malformed.iter().map(|bad| {
        format!(
            "registry — {} could not be read ({}); all-project commands skip project '{}'",
            bad.path.display(),
            bad.error,
            bad.name
        )
    });
    let unrestored = registry
        .projects
        .iter()
        .filter(|(_, entry)| entry.presence() == Presence::NotRestored)
        .map(|(name, entry)| {
            format!(
                "registry — {} holds no main checkout: `pm restore {name}` or `pm delete {name}`",
                entry.root_path().display()
            )
        });
    Ok(unreadable.chain(unrestored).collect())
}

/// Warn when the shared agent baseline is installed for this project but a
/// harness in use can't deliver pm's composed prompt — how the baseline
/// reaches an agent at spawn time. Nothing when the baseline isn't installed
/// (nothing to apply) or a binary can't be probed.
pub(super) fn baseline_capability_warnings(
    project_root: &Path,
    probe: Probe,
) -> Result<Vec<String>> {
    if !crate::commands::skills::baseline_path(project_root).exists() {
        return Ok(Vec::new());
    }
    let config = harness_config(Some(project_root));
    Ok(skills::harnesses_in_use(project_root)?
        .into_iter()
        // An unusable harness is reported as a finding of its own.
        .filter(|h| h.unusable_reason(&config, probe).is_none())
        .filter(|h| h.supports_prompt_delivery(&config, probe) == Some(false))
        .map(|h| format!("baseline — {}", prompt_delivery_unsupported(h)))
        .collect())
}

fn prompt_delivery_unsupported(harness: Harness) -> String {
    format!(
        "{harness} does not support {}; the shared agent baseline \
         (~/.agents/pm-baseline.md) will NOT be applied to spawned agents. Check your \
         {harness} version.",
        harness.prompt_mechanism()
    )
}

/// One line for `pm harness probe`: whether the installed binary supports
/// the capabilities pm relies on.
pub fn probe_line(harness: Harness, project_root: Option<&Path>) -> String {
    let mechanism = harness.prompt_mechanism();
    match harness.supports_prompt_delivery(&harness_config(project_root), Probe::Fresh) {
        Some(true) => format!("{harness}: {mechanism} supported — the shared baseline is applied"),
        Some(false) => format!(
            "{harness}: does not support {mechanism} — the shared agent baseline will not be \
             applied to spawned agents"
        ),
        None => format!("{harness}: binary not found (or probing it failed); nothing to probe"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn capability_warning_skipped_when_baseline_absent() {
        // No `pm-baseline.md` installed → the capability probe is never run
        // and no warning is produced (so `claude` isn't invoked needlessly).
        let dir = tempdir().unwrap();
        let project_root = dir.path();
        std::fs::create_dir_all(paths::main_worktree(project_root).join(".claude")).unwrap();
        assert!(
            baseline_capability_warnings(project_root, Probe::Fresh)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn global_config_warning_flags_unparseable_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[project\nmax_features =").unwrap();
        let warning = global_config_warning_in(dir.path()).expect("malformed config must warn");
        // Names both the file and what the user silently lost.
        assert!(warning.contains("config.toml"), "got: {warning}");
        assert!(warning.contains("[agents.models]"), "got: {warning}");
    }

    #[test]
    fn registry_warnings_name_each_unreadable_entry_only() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let good = ProjectEntry {
            root: "/tmp/good".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        good.save(&projects_dir, "good").unwrap();
        let bad = projects_dir.join("bad.toml");
        std::fs::write(&bad, "root = ").unwrap();

        let warnings = registry_warnings(&projects_dir).unwrap();

        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert!(
            warnings[0].contains(&bad.display().to_string()) && warnings[0].contains("'bad'"),
            "got: {}",
            warnings[0]
        );
    }

    #[test]
    fn registry_warnings_name_a_root_without_main_but_not_a_missing_root() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let husk = dir.path().join("husk");
        std::fs::create_dir_all(husk.join(".pm/messages")).unwrap();
        let here = dir.path().join("here");
        std::fs::create_dir_all(paths::main_worktree(&here)).unwrap();
        for (name, root) in [
            ("husk", husk.clone()),
            ("here", here),
            ("gone", dir.path().join("gone")),
        ] {
            ProjectEntry {
                root: root.to_string_lossy().into_owned(),
                main_branch: "main".to_string(),
                repo_url: None,
                state_remote: None,
            }
            .save(&projects_dir, name)
            .unwrap();
        }

        let warnings = registry_warnings(&projects_dir).unwrap();

        assert_eq!(
            warnings,
            [format!(
                "registry — {} holds no main checkout: `pm restore husk` or `pm delete husk`",
                husk.display()
            )]
        );
    }

    #[test]
    fn global_config_warning_silent_when_valid_or_absent() {
        let dir = tempdir().unwrap();
        assert!(global_config_warning_in(dir.path()).is_none());
        std::fs::write(
            dir.path().join("config.toml"),
            "[agents.models]\nreviewer = \"opus\"\n",
        )
        .unwrap();
        assert!(global_config_warning_in(dir.path()).is_none());
    }
}
