//! Stripping the entries earlier releases wrote into the project-level
//! `main/.claude/settings.json` and its seeded feature copies.

use std::path::{Path, PathBuf};

use super::settings::{load_settings, strip_pm_entries, write_settings};
use crate::commands::skills::worktrees_on_disk;
use crate::error::Result;
use crate::harness::Harness;

/// Remove pm-owned entries from the project-level settings file of main and
/// every feature worktree on disk. Returns the files changed (or that would
/// be). A file that can't be read or parsed is skipped: it can't hold a pm
/// entry we could safely edit, and it must not take `pm doctor`/`pm upgrade`
/// down.
pub(super) fn strip_project(project_root: &Path, dry_run: bool) -> Result<Vec<PathBuf>> {
    let mut changed = Vec::new();
    for path in project_settings_files(project_root)? {
        let Some(mut root) = load_settings(&path).ok().flatten() else {
            continue;
        };
        if !strip_pm_entries(&mut root) {
            continue;
        }
        if !dry_run {
            write_settings(&path, &root)?;
        }
        changed.push(path);
    }
    Ok(changed)
}

/// Project-level settings files that still carry a pm-owned hook entry.
pub fn stale_project_files(project_root: &Path) -> Result<Vec<PathBuf>> {
    strip_project(project_root, true)
}

fn project_settings_files(project_root: &Path) -> Result<Vec<PathBuf>> {
    Ok(worktrees_on_disk(project_root)?
        .into_iter()
        .map(|wt| {
            wt.join(Harness::ClaudeCode.config_dir())
                .join("settings.json")
        })
        .filter(|p| p.is_file())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::hooks_install::test_support::*;
    use crate::commands::hooks_install::{STOP_HOOK_TIMEOUT_SECS, install_in};
    use crate::state::paths;
    use serde_json::{Value, json};
    use std::fs;

    fn legacy_project_settings() -> Value {
        json!({
            "permissions": {"allow": ["Read"]},
            "hooks": {
                "Stop": [
                    {"hooks": [{"type": "command", "command": "echo foreign stop"}]},
                    {"hooks": [{"type": "command", "command": "pm harness hooks stop", "timeout": STOP_HOOK_TIMEOUT_SECS}]}
                ],
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "pm harness hooks session-start"}]}
                ]
            }
        })
    }

    #[test]
    fn install_migrates_project_files_to_the_user_level() {
        let (_dir, home, root) = setup();
        let main_file = paths::main_worktree(&root).join(".claude/settings.json");
        write_json(&main_file, &legacy_project_settings());
        // A feature worktree with a seeded copy.
        fs::create_dir_all(root.join(".pm/features")).unwrap();
        fs::write(
            root.join(".pm/features/login.toml"),
            "status = \"wip\"\nbranch = \"login\"\nworktree = \"login\"\ncreated = \"2026-01-01T00:00:00Z\"\nlast_active = \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
        let feat_file = root.join("login/.claude/settings.json");
        write_json(&feat_file, &legacy_project_settings());
        assert_eq!(stale_project_files(&root).unwrap().len(), 2);

        let dry = install_dry_run_in(&home, &root);
        assert_eq!(dry.len(), 5, "{dry:?}");
        assert!(dry[0].starts_with("Would install pm hooks in "), "{dry:?}");
        assert!(dry[1].starts_with("Would install pm hooks in "), "{dry:?}");
        assert!(
            dry.contains(&"Would remove pm hooks from main/.claude/settings.json".to_string()),
            "{dry:?}"
        );
        assert!(
            dry.contains(&"Would remove pm hooks from login/.claude/settings.json".to_string()),
            "{dry:?}"
        );
        assert!(!home.exists(), "dry-run wrote the user file");
        assert_eq!(
            read_json(&main_file),
            legacy_project_settings(),
            "dry-run wrote"
        );

        let lines = install_in(&home, Some(&root), false).unwrap();
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(claude_installed_in(&home).unwrap());

        for path in [&main_file, &feat_file] {
            let parsed = read_json(path);
            assert_eq!(
                parsed["permissions"]["allow"][0],
                "Read",
                "{}",
                path.display()
            );
            let stop = parsed["hooks"]["Stop"].as_array().unwrap();
            assert_eq!(stop.len(), 1, "{}", path.display());
            assert_eq!(
                command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
                "echo foreign stop"
            );
            assert!(parsed["hooks"].get("SessionStart").is_none(), "{parsed}");
        }
        assert!(stale_project_files(&root).unwrap().is_empty());
        assert!(install_in(&home, Some(&root), false).unwrap().is_empty());
    }

    #[test]
    fn strip_skips_a_malformed_project_file() {
        let (_dir, home, root) = setup();
        let main_file = paths::main_worktree(&root).join(".claude/settings.json");
        fs::create_dir_all(main_file.parent().unwrap()).unwrap();
        fs::write(&main_file, "{not json").unwrap();

        assert!(stale_project_files(&root).unwrap().is_empty());
        let lines = install_in(&home, Some(&root), false).unwrap();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(claude_installed_in(&home).unwrap());
        assert_eq!(fs::read_to_string(&main_file).unwrap(), "{not json");
    }
}
