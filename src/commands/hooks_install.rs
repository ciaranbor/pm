//! Install pm hooks (Stop + SessionStart) into the user-level hooks file of
//! every supported harness (`~/.claude/settings.json`, `$CODEX_HOME/hooks.json`
//! — both take the same nested `hooks` shape) — once per machine — and strip
//! the entries earlier releases wrote into `main/.claude/settings.json` and
//! its seeded feature copies. Both halves are idempotent, so `pm init`,
//! `pm upgrade`, `pm harness hooks install` and `pm doctor --fix` all run
//! them unconditionally; the global upsert runs first so a live session is
//! never left without the hook mid-migration (Claude Code merges the user
//! and project files and runs a duplicated handler once).
//!
//! Every supported harness, not only those in use, creating `$CODEX_HOME`
//! if absent: which harnesses are in use is a per-project answer and the
//! install also runs outside any project, so a harness named only in some
//! project's `.pm/config.toml` would otherwise be skipped and its agents
//! would idle silently. The accepted cost is codex's one-time trust prompt
//! in whichever codex session comes first, pm-spawned or not. `pm doctor`
//! checks only the harnesses the project's agents run on.
//!
//! A user-level hook fires in every session of that harness on the machine,
//! so each installed command is guarded on `PM_AGENT_NAME`: a non-pm session
//! exits 0 before `pm` is ever resolved, which also keeps the hook inert
//! when `pm` is not on that session's `PATH`. The guard is `[ -n … ] || exit
//! 0; pm …`, not `&&` — `&&` would turn a false test into an exit-1 hook
//! error. Codex additionally runs no hook until the user has trusted it
//! interactively (`pm doctor` reports a missing trust entry), and pm appends
//! its entries so existing ones keep their positions — codex keys trust on
//! the entry's index.
//!
//! The Stop hook is `pm harness hooks stop`, which blocks until the agent has
//! unread messages (by calling `agent_wait` internally), then returns
//! `{"decision":"block","reason":"You have new messages…"}`. Claude Code
//! delivers the reason as a continuation prompt, the agent reads its
//! messages, the turn ends, and the hook fires again.
//!
//! The SessionStart hook is `pm harness hooks session-start`, which captures
//! the session ID from the harness's JSON input and writes it to the agent
//! registry so dead agents can be resumed, and on codex also prints the
//! agent's composed prompt as `additionalContext`.
//!
//! Entries written by older releases (`pm claude hooks …`, or the unguarded
//! `pm harness hooks …`) are recognised as pm-owned: rewritten in place in
//! the user file, removed from project files.
//!
//! `{"decision":"block"}` loops indefinitely across real turns (verified
//! over 82 consecutive turns with no hard cap); `stop_hook_active` is
//! advisory or auto-resetting.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::commands::skills::worktrees_on_disk;
use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::harness::Harness;
use crate::state::paths;

/// Timeout in seconds for the Stop hook. Both harnesses honour the same
/// `timeout` key and default to 600s, which is too short for agents that
/// block waiting for messages; 24 hours gives ample headroom.
pub const STOP_HOOK_TIMEOUT_SECS: u64 = 86400;

/// Marker string used to identify pm-owned Stop hook entries in
/// settings.json.
pub const PM_HOOK_MARKER: &str = "pm harness hooks stop";

/// Marker string for pm-owned SessionStart hook entries.
pub const PM_SESSION_START_MARKER: &str = "pm harness hooks session-start";

/// The previous generation's markers, still treated as pm-owned so an
/// upgrade rewrites them in place rather than adding a second entry.
const LEGACY_HOOK_MARKER: &str = "pm claude hooks stop";
const LEGACY_SESSION_START_MARKER: &str = "pm claude hooks session-start";

const STOP_MARKERS: &[&str] = &[PM_HOOK_MARKER, LEGACY_HOOK_MARKER];
const SESSION_START_MARKERS: &[&str] = &[PM_SESSION_START_MARKER, LEGACY_SESSION_START_MARKER];

/// The hook events pm installs, with the command markers that identify
/// pm's entry under each.
pub const PM_EVENTS: &[(&str, &[&str])] = &[
    ("Stop", STOP_MARKERS),
    ("SessionStart", SESSION_START_MARKERS),
];

/// Shell prefix that makes a hook exit 0 outside pm agent sessions. `||`
/// rather than `&&`: a false test must not produce a non-zero exit, which
/// Claude Code would surface as a hook error.
const GUARD: &str = "[ -n \"$PM_AGENT_NAME\" ] || exit 0; ";

/// The shell command registered as the Stop hook. It blocks until unread
/// messages are available, printing the JSON decision to stdout.
pub fn stop_hook_command() -> String {
    format!("{GUARD}{PM_HOOK_MARKER}")
}

/// The shell command registered as the SessionStart hook.
pub fn session_start_hook_command() -> String {
    format!("{GUARD}{PM_SESSION_START_MARKER}")
}

/// The user-level file `harness`'s pm hooks live in.
pub fn user_settings_path(harness: Harness, home: &Path) -> Result<PathBuf> {
    harness.user_settings_file(home).ok_or_else(|| {
        PmError::Io(std::io::Error::other(format!(
            "{harness} has no user-level settings file"
        )))
    })
}

/// Install pm hooks into the user-level file of every supported harness
/// and, when inside a project, strip pm's entries from its project-level
/// files. Returns a human-readable status, one line per file changed.
pub fn install(project_root: Option<&Path>) -> Result<String> {
    let home = paths::home_dir()?;
    let lines = install_in(&home, project_root, false)?;
    if lines.is_empty() {
        let files: Vec<String> = Harness::SUPPORTED
            .iter()
            .map(|h| Ok(user_settings_path(*h, &home)?.display().to_string()))
            .collect::<Result<_>>()?;
        return Ok(format!(
            "pm hooks already installed in {}",
            files.join(", ")
        ));
    }
    Ok(lines.join("\n"))
}

/// Dry-run variant of [`install`]: one `Would …` line per file that would
/// change; empty when everything is up to date.
pub fn install_dry_run(project_root: Option<&Path>) -> Result<Vec<String>> {
    install_in(&paths::home_dir()?, project_root, true)
}

/// [`install`] against an explicit `home`, for tests that must not share
/// the per-binary test home. Returns one line per file changed (or, with
/// `dry_run`, per file that would change).
fn install_in(home: &Path, project_root: Option<&Path>, dry_run: bool) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for harness in Harness::SUPPORTED {
        let user_file = user_settings_path(*harness, home)?;
        if install_global(&user_file, dry_run)? {
            lines.push(format!(
                "{} pm hooks in {}",
                if dry_run {
                    "Would install"
                } else {
                    "Installed"
                },
                user_file.display()
            ));
        }
    }
    if let Some(root) = project_root {
        for path in strip_project(root, dry_run)? {
            lines.push(format!(
                "{} pm hooks from {}",
                if dry_run { "Would remove" } else { "Removed" },
                path.strip_prefix(root).unwrap_or(&path).display()
            ));
        }
    }
    Ok(lines)
}

/// Upsert both pm entries into the user-level file. Returns whether the
/// file changed (or would).
fn install_global(user_file: &Path, dry_run: bool) -> Result<bool> {
    let mut root =
        load_settings(user_file)?.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let stop_changed = upsert_stop_hook(&mut root)?;
    let session_start_changed = upsert_session_start_hook(&mut root)?;
    if !(stop_changed || session_start_changed) {
        return Ok(false);
    }
    if !dry_run {
        write_settings(user_file, &root)?;
    }
    Ok(true)
}

/// Remove pm-owned entries from the project-level settings file of main and
/// every feature worktree on disk. Returns the files changed (or that would
/// be). A file that can't be read or parsed is skipped: it can't hold a pm
/// entry we could safely edit, and it must not take `pm doctor`/`pm upgrade`
/// down.
fn strip_project(project_root: &Path, dry_run: bool) -> Result<Vec<PathBuf>> {
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

/// Parse a settings file. `None` when it is missing or empty; an error when
/// it isn't a JSON object (never clobber a file we don't understand).
fn load_settings(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    let parsed: Value = serde_json::from_str(&content).map_err(|e| {
        PmError::Io(std::io::Error::other(format!(
            "{}: invalid JSON: {e}",
            path.display()
        )))
    })?;
    if !parsed.is_object() {
        return Err(PmError::Io(std::io::Error::other(format!(
            "{}: root must be a JSON object",
            path.display()
        ))));
    }
    Ok(Some(parsed))
}

fn write_settings(path: &Path, root: &Value) -> Result<()> {
    let serialized = serde_json::to_string_pretty(root)
        .map_err(|e| PmError::Io(std::io::Error::other(e.to_string())))?;
    write_atomic(path, format!("{serialized}\n").as_bytes())
}

/// `hooks.<event>` as a mutable array, creating it when absent. Errors when
/// `hooks` or the event slot holds something other than the expected shape.
fn event_array<'a>(root: &'a mut Value, event: &str) -> Result<&'a mut Vec<Value>> {
    let obj = root.as_object_mut().expect("validated in load_settings");
    let hooks_entry = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !hooks_entry.is_object() {
        return Err(PmError::Io(std::io::Error::other(
            "settings.json `hooks` must be an object",
        )));
    }
    let entry = hooks_entry
        .as_object_mut()
        .unwrap()
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    entry.as_array_mut().ok_or_else(|| {
        PmError::Io(std::io::Error::other(format!(
            "settings.json `hooks.{event}` must be an array"
        )))
    })
}

/// Replace the pm-owned inner hook in `hooks.<event>` (any generation) with
/// `pm_hook` in place — so a foreign hook the user bundled into the same
/// entry survives — or append a new entry holding it. Returns `true` when
/// the value changed.
fn upsert_hook(root: &mut Value, event: &str, markers: &[&str], pm_hook: Value) -> Result<bool> {
    let array = event_array(root, event)?;
    for entry in array.iter_mut() {
        let Some(inner) = entry.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        if let Some(idx) = inner.iter().position(|h| command_matches(h, markers)) {
            if inner[idx] == pm_hook {
                return Ok(false);
            }
            inner[idx] = pm_hook;
            return Ok(true);
        }
    }
    array.push(json!({ "hooks": [pm_hook] }));
    Ok(true)
}

fn upsert_stop_hook(root: &mut Value) -> Result<bool> {
    let hook = json!({
        "type": "command",
        "command": stop_hook_command(),
        "timeout": STOP_HOOK_TIMEOUT_SECS,
    });
    upsert_hook(root, "Stop", STOP_MARKERS, hook)
}

fn upsert_session_start_hook(root: &mut Value) -> Result<bool> {
    let hook = json!({
        "type": "command",
        "command": session_start_hook_command(),
    });
    upsert_hook(root, "SessionStart", SESSION_START_MARKERS, hook)
}

/// Remove every pm-owned hook from `hooks.Stop`/`hooks.SessionStart`,
/// pruning an emptied entry, event array and `hooks` object. Only the pm
/// inner hook is removed, so a foreign hook bundled into the same entry
/// survives. Returns `true` when the value changed.
fn strip_pm_entries(root: &mut Value) -> bool {
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return false;
    };
    let mut changed = false;
    for &(event, markers) in PM_EVENTS {
        let Some(array) = hooks.get_mut(event).and_then(|v| v.as_array_mut()) else {
            continue;
        };
        for entry in array.iter_mut() {
            let Some(inner) = entry.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
                continue;
            };
            let before = inner.len();
            inner.retain(|hook| !command_matches(hook, markers));
            changed |= inner.len() != before;
        }
        array.retain(|entry| {
            entry
                .get("hooks")
                .and_then(|v| v.as_array())
                .is_none_or(|inner| !inner.is_empty())
        });
        if array.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        root.as_object_mut().unwrap().remove("hooks");
    }
    changed
}

fn command_matches(hook: &Value, markers: &[&str]) -> bool {
    hook.get("command")
        .and_then(|v| v.as_str())
        .is_some_and(|cmd| markers.iter().any(|m| cmd.contains(m)))
}

/// Whether both pm entries are present in `harness`'s user-level file.
pub fn is_installed_for(harness: Harness) -> Result<bool> {
    is_installed_in(harness, &paths::home_dir()?)
}

/// [`is_installed_for`] against an explicit `home`.
fn is_installed_in(harness: Harness, home: &Path) -> Result<bool> {
    let Some(parsed) = user_hooks_root(harness, home)? else {
        return Ok(false);
    };
    Ok(PM_EVENTS
        .iter()
        .all(|(event, markers)| pm_hook_position(&parsed, event, markers).is_some()))
}

/// The parsed user-level hooks file of `harness`; `None` when it is missing
/// or not JSON.
pub fn user_hooks_root(harness: Harness, home: &Path) -> Result<Option<Value>> {
    let path = user_settings_path(harness, home)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str::<Value>(&content).ok())
}

/// Where pm's hook sits under `hooks.<event>`: the entry index and the
/// inner-hook index — the coordinates codex keys hook trust on.
pub fn pm_hook_position(root: &Value, event: &str, markers: &[&str]) -> Option<(usize, usize)> {
    let entries = root.get("hooks")?.get(event)?.as_array()?;
    entries.iter().enumerate().find_map(|(i, entry)| {
        let inner = entry.get("hooks")?.as_array()?;
        let j = inner.iter().position(|h| command_matches(h, markers))?;
        Some((i, j))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::{TempDir, tempdir};

    /// An isolated home and a project root under one tempdir.
    fn setup() -> (TempDir, PathBuf, PathBuf) {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        let root = dir.path().join("proj");
        fs::create_dir_all(paths::main_worktree(&root)).unwrap();
        (dir, home, root)
    }

    fn user_file(home: &Path) -> PathBuf {
        home.join(".claude/settings.json")
    }

    fn codex_file(home: &Path) -> PathBuf {
        home.join(".codex/hooks.json")
    }

    fn is_installed_in(home: &Path) -> Result<bool> {
        super::is_installed_in(Harness::ClaudeCode, home)
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn write_json(path: &Path, value: &Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    }

    fn command_at(root: &Value, pointer: &str) -> String {
        root.pointer(pointer)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("no string at {pointer} in {root}"))
            .to_string()
    }

    fn legacy_project_settings() -> Value {
        json!({
            "permissions": {"allow": ["Read"]},
            "hooks": {
                "Stop": [
                    {"hooks": [{"type": "command", "command": "echo foreign stop"}]},
                    {"hooks": [{"type": "command", "command": "pm claude hooks stop", "timeout": STOP_HOOK_TIMEOUT_SECS}]}
                ],
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "pm claude hooks session-start"}]}
                ]
            }
        })
    }

    #[test]
    fn install_creates_user_file_on_fresh_machine() {
        let (_dir, home, root) = setup();
        assert!(!home.exists());

        let lines = install_in(&home, Some(&root), false).unwrap();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("Installed pm hooks in "), "{lines:?}");
        assert!(
            lines[1].ends_with(&codex_file(&home).display().to_string()),
            "{lines:?}"
        );

        let parsed = read_json(&user_file(&home));
        assert_eq!(parsed["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(
            command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
            stop_hook_command()
        );
        assert_eq!(
            parsed
                .pointer("/hooks/Stop/0/hooks/0/timeout")
                .and_then(|v| v.as_u64()),
            Some(STOP_HOOK_TIMEOUT_SECS)
        );
        assert_eq!(parsed["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(
            command_at(&parsed, "/hooks/SessionStart/0/hooks/0/command"),
            session_start_hook_command()
        );
        assert!(is_installed_in(&home).unwrap());
        // Every supported harness, whether or not it is installed or
        // configured anywhere: a project may name it later.
        assert!(super::is_installed_in(Harness::Codex, &home).unwrap());
        assert_eq!(
            read_json(&codex_file(&home))["hooks"],
            parsed["hooks"],
            "same nested shape in both files"
        );
        // A fresh project gets no project-level file.
        assert!(!paths::main_worktree(&root).join(".claude").exists());
    }

    #[test]
    fn install_appends_after_a_codex_users_own_hooks() {
        let (_dir, home, root) = setup();
        // A codex user with a hook of their own, at index 0.
        let codex_hooks = codex_file(&home);
        write_json(
            &codex_hooks,
            &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}}),
        );

        let lines = install_in(&home, Some(&root), false).unwrap();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[1].ends_with(&codex_hooks.display().to_string()),
            "{lines:?}"
        );
        assert!(super::is_installed_in(Harness::Codex, &home).unwrap());
        assert!(is_installed_in(&home).unwrap());

        let parsed = read_json(&codex_hooks);
        // The user's hook keeps position 0 (codex keys its trust on the
        // index); pm's entry follows.
        assert_eq!(
            command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
            "say done"
        );
        assert_eq!(
            pm_hook_position(&parsed, "Stop", STOP_MARKERS),
            Some((1, 0))
        );
        assert_eq!(
            pm_hook_position(&parsed, "SessionStart", SESSION_START_MARKERS),
            Some((0, 0))
        );
        assert_eq!(
            command_at(&parsed, "/hooks/Stop/1/hooks/0/command"),
            stop_hook_command()
        );
        assert_eq!(
            parsed
                .pointer("/hooks/Stop/1/hooks/0/timeout")
                .and_then(|v| v.as_u64()),
            Some(STOP_HOOK_TIMEOUT_SECS)
        );
        assert!(Harness::Codex.malformed_hook_events(&parsed).is_empty());

        // Idempotent across both files.
        assert!(install_in(&home, Some(&root), true).unwrap().is_empty());
    }

    #[test]
    fn install_ignores_the_project_config() {
        // Which harnesses a project uses is not consulted, so a malformed
        // config can't block the install or narrow it.
        let (_dir, home, root) = setup();
        let pm_dir = root.join(".pm");
        fs::create_dir_all(&pm_dir).unwrap();
        fs::write(
            pm_dir.join("config.toml"),
            "[agents.harness]\n[agents.harness]\n",
        )
        .unwrap();

        let lines = install_in(&home, Some(&root), false).unwrap();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(is_installed_in(&home).unwrap());
        assert!(super::is_installed_in(Harness::Codex, &home).unwrap());
    }

    #[test]
    fn install_preserves_the_users_own_settings_and_hooks() {
        let (_dir, home, _root) = setup();
        write_json(
            &user_file(&home),
            &json!({
                "model": "sonnet",
                "tui": {"theme": "dark"},
                "permissions": {"allow": ["Bash(ls:*)"]},
                "hooks": {
                    "Stop": [{"hooks": [{"type": "command", "command": "echo user stop"}]}],
                    "SessionStart": [{"hooks": [{"type": "command", "command": "echo user start"}]}]
                }
            }),
        );

        install_in(&home, None, false).unwrap();

        let parsed = read_json(&user_file(&home));
        assert_eq!(parsed["model"], "sonnet");
        assert_eq!(parsed["tui"]["theme"], "dark");
        assert_eq!(parsed["permissions"]["allow"][0], "Bash(ls:*)");
        let stop = parsed["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(
            command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
            "echo user stop"
        );
        assert_eq!(
            command_at(&parsed, "/hooks/Stop/1/hooks/0/command"),
            stop_hook_command()
        );
        let ss = parsed["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(ss.len(), 2);
        assert_eq!(
            command_at(&parsed, "/hooks/SessionStart/0/hooks/0/command"),
            "echo user start"
        );
    }

    #[test]
    fn install_is_idempotent() {
        let (_dir, home, root) = setup();
        install_in(&home, Some(&root), false).unwrap();
        let first = fs::read_to_string(user_file(&home)).unwrap();

        let lines = install_in(&home, Some(&root), false).unwrap();
        assert!(lines.is_empty(), "{lines:?}");
        assert_eq!(fs::read_to_string(user_file(&home)).unwrap(), first);
        assert!(install_dry_run_in(&home, &root).is_empty());
    }

    fn install_dry_run_in(home: &Path, root: &Path) -> Vec<String> {
        install_in(home, Some(root), true).unwrap()
    }

    #[test]
    fn install_rewrites_older_user_level_spellings_in_place() {
        for old in ["pm claude hooks stop", "pm harness hooks stop"] {
            let (_dir, home, _root) = setup();
            write_json(
                &user_file(&home),
                &json!({"hooks": {
                    "Stop": [{"hooks": [{"type": "command", "command": old}]}],
                    "SessionStart": [{"hooks": [{"type": "command", "command": old.replace("stop", "session-start")}]}]
                }}),
            );
            assert!(is_installed_in(&home).unwrap(), "{old}");

            let lines = install_in(&home, None, false).unwrap();
            assert_eq!(lines.len(), 2, "{old}: {lines:?}");
            assert!(lines[0].ends_with(&user_file(&home).display().to_string()));

            let parsed = read_json(&user_file(&home));
            assert_eq!(
                parsed["hooks"]["Stop"].as_array().unwrap().len(),
                1,
                "{old}"
            );
            assert_eq!(
                command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
                stop_hook_command()
            );
            assert_eq!(
                parsed
                    .pointer("/hooks/Stop/0/hooks/0/timeout")
                    .and_then(|v| v.as_u64()),
                Some(STOP_HOOK_TIMEOUT_SECS)
            );
            assert_eq!(parsed["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
            assert_eq!(
                command_at(&parsed, "/hooks/SessionStart/0/hooks/0/command"),
                session_start_hook_command()
            );
        }
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
        assert_eq!(dry.len(), 4, "{dry:?}");
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
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(is_installed_in(&home).unwrap());

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
    fn strip_prunes_emptied_hooks_object_and_keeps_bundled_foreign_hook() {
        let mut only_pm = json!({"hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "pm harness hooks stop"}]}],
            "SessionStart": [{"hooks": [{"type": "command", "command": "pm harness hooks session-start"}]}]
        }, "model": "opus"});
        assert!(strip_pm_entries(&mut only_pm));
        assert_eq!(only_pm, json!({"model": "opus"}));

        let mut bundled = json!({"hooks": {"Stop": [{"matcher": "x", "hooks": [
            {"type": "command", "command": stop_hook_command()},
            {"type": "command", "command": "echo mine"}
        ]}]}});
        assert!(strip_pm_entries(&mut bundled));
        assert_eq!(
            bundled,
            json!({"hooks": {"Stop": [{"matcher": "x", "hooks": [
                {"type": "command", "command": "echo mine"}
            ]}]}})
        );

        let mut foreign = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "pm msg wait && notify"}]}]}});
        let before = foreign.clone();
        assert!(!strip_pm_entries(&mut foreign));
        assert_eq!(foreign, before);
    }

    #[test]
    fn foreign_msg_wait_hook_is_not_pm_owned() {
        // The two-generations-old `pm msg wait` shape is no longer claimed:
        // a user's own hook wrapping it is left alone and not counted.
        let (_dir, home, _root) = setup();
        write_json(
            &user_file(&home),
            &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "pm msg wait && notify"}]}]}}),
        );
        assert!(!is_installed_in(&home).unwrap());

        install_in(&home, None, false).unwrap();
        let parsed = read_json(&user_file(&home));
        let stop = parsed["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(
            command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
            "pm msg wait && notify"
        );
    }

    #[test]
    fn is_installed_false_with_foreign_stop_hook_only() {
        let (_dir, home, _root) = setup();
        assert!(!is_installed_in(&home).unwrap());
        write_json(
            &user_file(&home),
            &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "echo foreign"}]}]}}),
        );
        assert!(!is_installed_in(&home).unwrap());
    }

    #[test]
    fn install_keeps_a_foreign_hook_bundled_with_pms() {
        let (_dir, home, _root) = setup();
        write_json(
            &user_file(&home),
            &json!({"hooks": {"Stop": [{"matcher": "x", "hooks": [
                {"type": "command", "command": "pm claude hooks stop"},
                {"type": "command", "command": "my-notify"}
            ]}]}}),
        );

        install_in(&home, None, false).unwrap();

        let parsed = read_json(&user_file(&home));
        let stop = parsed["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1, "{parsed}");
        assert_eq!(stop[0]["matcher"], "x");
        assert_eq!(
            command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
            stop_hook_command()
        );
        assert_eq!(
            command_at(&parsed, "/hooks/Stop/0/hooks/1/command"),
            "my-notify"
        );
    }

    #[test]
    fn strip_skips_a_malformed_project_file() {
        let (_dir, home, root) = setup();
        let main_file = paths::main_worktree(&root).join(".claude/settings.json");
        fs::create_dir_all(main_file.parent().unwrap()).unwrap();
        fs::write(&main_file, "{not json").unwrap();

        assert!(stale_project_files(&root).unwrap().is_empty());
        let lines = install_in(&home, Some(&root), false).unwrap();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(is_installed_in(&home).unwrap());
        assert_eq!(fs::read_to_string(&main_file).unwrap(), "{not json");
    }

    #[test]
    fn install_refuses_a_malformed_user_file() {
        let (_dir, home, _root) = setup();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(user_file(&home), "[]").unwrap();
        assert!(install_in(&home, None, false).is_err());
        assert_eq!(fs::read_to_string(user_file(&home)).unwrap(), "[]");
    }

    #[test]
    fn installed_commands_are_inert_outside_pm_sessions() {
        // The real installed strings, run the way Claude Code runs them:
        // without PM_AGENT_NAME they exit 0 with no output and never reach
        // `pm` (PATH is emptied so a resolution attempt would fail).
        for command in [stop_hook_command(), session_start_hook_command()] {
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", &command])
                .env_remove("PM_AGENT_NAME")
                .env("PATH", "")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(out.status.success(), "{command}: {out:?}");
            assert!(out.stdout.is_empty(), "{command}: {out:?}");
            assert!(out.stderr.is_empty(), "{command}: {out:?}");
        }
    }

    #[test]
    fn installed_commands_reach_pm_inside_pm_sessions() {
        // With PM_AGENT_NAME set the guard falls through to `pm`, resolved
        // from PATH — here a stub that echoes its arguments.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let stub = dir.path().join("pm");
        fs::write(&stub, "#!/bin/sh\necho \"stub $*\"\n").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

        for (command, args) in [
            (stop_hook_command(), "harness hooks stop"),
            (session_start_hook_command(), "harness hooks session-start"),
        ] {
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", &command])
                .env("PM_AGENT_NAME", "x")
                .env("PATH", dir.path())
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(out.status.success(), "{command}: {out:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                format!("stub {args}\n")
            );
        }
    }
}
