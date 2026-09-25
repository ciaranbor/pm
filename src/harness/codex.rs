//! Codex: `codex -a <policy> -s <sandbox> … [resume|fork <id>] [prompt]`.
//! `-a never` is the default because an approval prompt in an unwatched tmux
//! window stalls the agent.
//!
//! Codex has no launch-time role channel (no `--agent`), so pm's composed
//! prompt — definition body, baseline, notice boards — is injected by the
//! SessionStart hook as `hookSpecificOutput.additionalContext`; SessionStart
//! also fires on `codex resume`, so the role re-applies without extra flags.
//! Hooks live in `$CODEX_HOME/hooks.json` in Claude Code's nested shape (a
//! flat shape parses and registers nothing). Codex reads skills from the
//! canonical `.agents/skills/` directly, so nothing is projected for it.
//!
//! Two trust gates in `$CODEX_HOME/config.toml` stand between a spawn and a
//! working agent: `[projects."<dir>"] trust_level = "trusted"`, which pm writes
//! per worktree before launching, and `[hooks.state."<hooks.json>:<event>:<i>:<j>"]
//! trusted_hash`, which only codex can write (one interactive "Trust all and
//! continue" per machine, re-asked when a hook's command text changes). Absent
//! hook trust, hooks silently do not run — `pm doctor` checks for the entry.
//!
//! pm's tmux socket is unreachable from inside any codex sandbox (macOS
//! Seatbelt blocks `AF_UNIX` connect), so the default sandbox is
//! `danger-full-access` — the same blast radius as pm's Claude Code agents.
//! A sandboxed agent (`workspace-write` + writable roots) can still read,
//! run git, and send messages; it cannot spawn, heal, or stop agents.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::harness::SpawnSpec;
use crate::state::project::CodexConfig;
use crate::tmux;

pub(super) const CONFIG_DIR: &str = ".codex";
pub(super) const HOOKS_FILE: &str = "hooks.json";
const CONFIG_FILE: &str = "config.toml";

/// Earliest release with the measured hook behaviour pm relies on.
pub(super) const MIN_VERSION: (u32, u32, u32) = (0, 153, 2);

/// The sandbox mode with nothing to open up.
const FULL_ACCESS: &str = "danger-full-access";
const DEFAULT_SANDBOX: &str = FULL_ACCESS;
const DEFAULT_APPROVAL: &str = "never";

pub(super) const HOOK_TRUST_REMEDY: &str = "start `codex` once in a trusted directory and \
    choose \"Trust all and continue\" (or set `[harness.codex] bypass_hook_trust = true`)";

/// `$CODEX_HOME`, else `<home>/.codex`. Tests always get the latter so a
/// developer's own `CODEX_HOME` never leaks into a test home.
pub(super) fn home_dir(home: &Path) -> PathBuf {
    #[cfg(not(test))]
    if let Some(dir) = std::env::var_os("CODEX_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    home.join(CONFIG_DIR)
}

fn config_file(codex_home: &Path) -> PathBuf {
    codex_home.join(CONFIG_FILE)
}

pub(super) fn build_cmd(spec: &SpawnSpec<'_>, cfg: &CodexConfig) -> String {
    let SpawnSpec {
        definition: _,
        append_prompt_file: _,
        prompt,
        resume_session,
        fork_session,
        permission_mode,
        model,
        writable_dirs,
    } = *spec;

    let approval = cfg.approval.as_deref().unwrap_or(DEFAULT_APPROVAL);
    // The per-agent permission row is the sandbox mode; `[harness.codex]`
    // supplies the harness-wide default.
    let sandbox = permission_mode
        .or(cfg.sandbox.as_deref())
        .unwrap_or(DEFAULT_SANDBOX);

    let mut parts = vec![
        "codex".to_string(),
        "-a".to_string(),
        tmux::shell_quote(approval),
        "-s".to_string(),
        tmux::shell_quote(sandbox),
    ];

    if cfg.bypass_hook_trust == Some(true) {
        parts.push("--dangerously-bypass-hook-trust".to_string());
    }

    if sandbox != FULL_ACCESS {
        for dir in writable_dirs {
            parts.push("--add-dir".to_string());
            parts.push(tmux::shell_quote(&dir.to_string_lossy()));
        }
    }

    if let Some(id) = model {
        parts.push("-m".to_string());
        parts.push(tmux::shell_quote(id));
    }

    // Subcommands come after the global options.
    if let Some(session_id) = resume_session {
        parts.push(if fork_session { "fork" } else { "resume" }.to_string());
        parts.push(session_id.to_string());
    }

    if let Some(p) = prompt {
        parts.push(tmux::shell_quote(p));
    }

    parts.join(" ")
}

/// The SessionStart hook's stdout: `context` becomes developer instructions
/// for the session being started or resumed.
pub(super) fn session_start_output(context: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": context,
        }
    })
    .to_string()
}

/// Whether `hooks.<event>` holds an entry codex would register nothing for:
/// a hook object placed directly in the event array instead of wrapped in
/// `{"hooks": [...]}`. Returns the offending event names.
pub(super) fn flat_hook_events(root: &Value) -> Vec<String> {
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
    };
    hooks
        .iter()
        .filter(|(_, entries)| {
            entries.as_array().is_some_and(|arr| {
                arr.iter()
                    .any(|e| e.get("hooks").and_then(Value::as_array).is_none())
            })
        })
        .map(|(event, _)| event.clone())
        .collect()
}

/// The `hooks.state` key codex trusts a hook under: the hooks file's
/// absolute path, the event in snake case, then the entry and inner-hook
/// indices within `hooks.<event>`.
pub(super) fn hook_trust_key(hooks_file: &Path, event: &str, entry: usize, hook: usize) -> String {
    format!(
        "{}:{}:{entry}:{hook}",
        hooks_file.display(),
        snake_case(event)
    )
}

fn snake_case(event: &str) -> String {
    let mut out = String::new();
    for (i, c) in event.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn load_config(codex_home: &Path) -> Result<DocumentMut> {
    let path = config_file(codex_home);
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    std::fs::read_to_string(&path)?
        .parse::<DocumentMut>()
        .map_err(|e| PmError::Io(std::io::Error::other(format!("{}: {e}", path.display()))))
}

fn table_at<'a>(doc: &'a DocumentMut, keys: &[&str]) -> Option<&'a dyn toml_edit::TableLike> {
    let mut cur: &dyn toml_edit::TableLike = doc.as_table();
    for key in keys {
        cur = cur.get(key)?.as_table_like()?;
    }
    Some(cur)
}

/// Whether codex already trusts `hooks.state.<key>`.
pub(super) fn hook_trusted(codex_home: &Path, key: &str) -> bool {
    let Ok(doc) = load_config(codex_home) else {
        return false;
    };
    table_at(&doc, &["hooks", "state", key])
        .and_then(|t| t.get("trusted_hash"))
        .and_then(Item::as_str)
        .is_some()
}

/// The path codex matches its cwd against: the canonical form, since the
/// TUI resolves symlinks (`/tmp` → `/private/tmp` on macOS).
fn trust_path(dir: &Path) -> String {
    dir.canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Whether `dir` is marked trusted, so the TUI skips its directory prompt.
pub(super) fn dir_trusted(codex_home: &Path, dir: &Path) -> bool {
    let Ok(doc) = load_config(codex_home) else {
        return false;
    };
    table_at(&doc, &["projects", &trust_path(dir)])
        .and_then(|t| t.get("trust_level"))
        .and_then(Item::as_str)
        == Some("trusted")
}

/// Mark `dir` trusted, preserving everything else in the file. Returns
/// whether the file changed.
pub(super) fn trust_dir(codex_home: &Path, dir: &Path) -> Result<bool> {
    if dir_trusted(codex_home, dir) {
        return Ok(false);
    }
    let mut doc = load_config(codex_home)?;
    let projects = doc.as_table_mut().entry("projects").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    let projects = projects.as_table_like_mut().ok_or_else(|| {
        PmError::Io(std::io::Error::other(
            "config.toml `projects` must be a table",
        ))
    })?;
    let key = trust_path(dir);
    let entry = projects.entry(&key).or_insert(Item::Table(Table::new()));
    let entry = entry.as_table_like_mut().ok_or_else(|| {
        PmError::Io(std::io::Error::other(format!(
            "config.toml `projects.\"{key}\"` must be a table"
        )))
    })?;
    entry.insert("trust_level", value("trusted"));
    write_atomic(&config_file(codex_home), doc.to_string().as_bytes())?;
    Ok(true)
}

/// `(major, minor, patch)` from `codex --version` output (`codex-cli 0.153.2`).
fn parse_version(output: &str) -> Option<(u32, u32, u32)> {
    let token = output.split_whitespace().last()?;
    let mut nums = token
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u32>().ok());
    Some((nums.next()??, nums.next()??, nums.next()??))
}

/// The installed version's raw string, or `None` when `codex` can't be run.
pub(super) fn installed_version() -> Option<String> {
    let out = std::process::Command::new("codex")
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether the installed codex is at least [`MIN_VERSION`]; `None` when it
/// can't be probed.
pub(super) fn version_supported() -> Option<bool> {
    Some(parse_version(&installed_version()?)? >= MIN_VERSION)
}

pub(super) fn min_version_string() -> String {
    let (a, b, c) = MIN_VERSION;
    format!("{a}.{b}.{c}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CodexConfig {
        CodexConfig::default()
    }

    #[test]
    fn build_cmd_defaults_to_never_and_full_access() {
        let cmd = build_cmd(
            &SpawnSpec {
                definition: Some("reviewer"),
                append_prompt_file: Some("/x/baseline.md"),
                prompt: Some("Stand by."),
                ..Default::default()
            },
            &cfg(),
        );
        // The definition and prompt file are delivered by the SessionStart
        // hook, never on the command line.
        assert_eq!(cmd, "codex -a 'never' -s 'danger-full-access' 'Stand by.'");
    }

    #[test]
    fn build_cmd_sandboxed_agent_gets_writable_dirs_and_permission_row_wins() {
        let dirs = vec![PathBuf::from("/proj/.pm"), PathBuf::from("/proj/main/.git")];
        let config = CodexConfig {
            sandbox: Some("read-only".into()),
            approval: Some("on-request".into()),
            ..Default::default()
        };
        let cmd = build_cmd(
            &SpawnSpec {
                permission_mode: Some("workspace-write"),
                model: Some("gpt-5"),
                writable_dirs: &dirs,
                ..Default::default()
            },
            &config,
        );
        assert_eq!(
            cmd,
            "codex -a 'on-request' -s 'workspace-write' --add-dir '/proj/.pm' \
             --add-dir '/proj/main/.git' -m 'gpt-5'"
        );

        // Without a permission row the harness-wide sandbox applies; under
        // full access the roots are redundant and omitted.
        let cmd = build_cmd(
            &SpawnSpec {
                writable_dirs: &dirs,
                ..Default::default()
            },
            &cfg(),
        );
        assert_eq!(cmd, "codex -a 'never' -s 'danger-full-access'");
    }

    #[test]
    fn build_cmd_resume_and_fork_are_subcommands_after_options() {
        let cmd = build_cmd(
            &SpawnSpec {
                resume_session: Some("abc"),
                prompt: Some("go"),
                ..Default::default()
            },
            &cfg(),
        );
        assert_eq!(
            cmd,
            "codex -a 'never' -s 'danger-full-access' resume abc 'go'"
        );
        let cmd = build_cmd(
            &SpawnSpec {
                resume_session: Some("abc"),
                fork_session: true,
                ..Default::default()
            },
            &cfg(),
        );
        assert_eq!(cmd, "codex -a 'never' -s 'danger-full-access' fork abc");
        // A fork without a source is a plain spawn.
        let cmd = build_cmd(
            &SpawnSpec {
                fork_session: true,
                ..Default::default()
            },
            &cfg(),
        );
        assert_eq!(cmd, "codex -a 'never' -s 'danger-full-access'");
    }

    #[test]
    fn build_cmd_bypass_hook_trust_is_opt_in() {
        let config = CodexConfig {
            bypass_hook_trust: Some(true),
            ..Default::default()
        };
        assert_eq!(
            build_cmd(&SpawnSpec::default(), &config),
            "codex -a 'never' -s 'danger-full-access' --dangerously-bypass-hook-trust"
        );
        let config = CodexConfig {
            bypass_hook_trust: Some(false),
            ..Default::default()
        };
        assert_eq!(
            build_cmd(&SpawnSpec::default(), &config),
            "codex -a 'never' -s 'danger-full-access'"
        );
    }

    #[test]
    fn session_start_output_is_the_hook_specific_shape() {
        let out: Value = serde_json::from_str(&session_start_output("role text")).unwrap();
        assert_eq!(
            out,
            json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "role text"}})
        );
    }

    #[test]
    fn hook_trust_key_matches_codex_config_layout() {
        assert_eq!(
            hook_trust_key(Path::new("/h/.codex/hooks.json"), "SessionStart", 1, 0),
            "/h/.codex/hooks.json:session_start:1:0"
        );
        assert_eq!(
            hook_trust_key(Path::new("/h/.codex/hooks.json"), "Stop", 0, 0),
            "/h/.codex/hooks.json:stop:0:0"
        );
    }

    #[test]
    fn flat_hook_events_flags_unwrapped_entries_only() {
        let nested = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "x"}]}]}});
        assert!(flat_hook_events(&nested).is_empty());
        let flat = json!({"hooks": {
            "Stop": [{"type": "command", "command": "x"}],
            "SessionStart": [{"hooks": [{"type": "command", "command": "y"}]}]
        }});
        assert_eq!(flat_hook_events(&flat), vec!["Stop".to_string()]);
        assert!(flat_hook_events(&json!({})).is_empty());
    }

    #[test]
    fn hook_trusted_reads_the_state_table() {
        let home = tempfile::tempdir().unwrap();
        let hooks = home.path().join("hooks.json");
        let key = hook_trust_key(&hooks, "Stop", 0, 0);
        assert!(!hook_trusted(home.path(), &key));
        std::fs::write(
            config_file(home.path()),
            format!("[hooks.state.\"{key}\"]\ntrusted_hash = \"sha256:abc\"\n"),
        )
        .unwrap();
        assert!(hook_trusted(home.path(), &key));
        assert!(!hook_trusted(
            home.path(),
            &hook_trust_key(&hooks, "SessionStart", 0, 0)
        ));
    }

    #[test]
    fn trust_dir_adds_an_entry_and_preserves_the_rest_of_the_file() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let canonical = work.path().canonicalize().unwrap();
        let existing = "# my codex config\nmodel = \"gpt-5\"   # keep me\n\n[projects.\"/elsewhere\"]\ntrust_level = \"trusted\"\n";
        std::fs::write(config_file(home.path()), existing).unwrap();

        assert!(!dir_trusted(home.path(), work.path()));
        assert!(trust_dir(home.path(), work.path()).unwrap());
        assert!(dir_trusted(home.path(), work.path()));
        assert!(dir_trusted(home.path(), &canonical));

        let written = std::fs::read_to_string(config_file(home.path())).unwrap();
        assert!(written.starts_with(existing), "{written}");
        let doc: DocumentMut = written.parse().unwrap();
        assert_eq!(
            doc["projects"][canonical.to_string_lossy().as_ref()]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(
            doc["projects"]["/elsewhere"]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(doc["model"].as_str(), Some("gpt-5"));

        // Idempotent.
        assert!(!trust_dir(home.path(), work.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(config_file(home.path())).unwrap(),
            written
        );
    }

    #[test]
    fn trust_dir_creates_a_missing_config() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        assert!(trust_dir(home.path(), work.path()).unwrap());
        let written = std::fs::read_to_string(config_file(home.path())).unwrap();
        let doc: DocumentMut = written.parse().unwrap();
        let key = work.path().canonicalize().unwrap();
        assert_eq!(
            doc["projects"][key.to_string_lossy().as_ref()]["trust_level"].as_str(),
            Some("trusted")
        );
        // A header per project, as codex itself writes it — not a dotted
        // inline table.
        assert!(written.starts_with("[projects.\""), "{written}");
    }

    #[test]
    fn parse_version_reads_codex_cli_output() {
        assert_eq!(parse_version("codex-cli 0.153.2"), Some((0, 153, 2)));
        assert_eq!(parse_version("codex-cli 1.2.0-alpha.3\n"), Some((1, 2, 0)));
        assert_eq!(parse_version("codex-cli"), None);
        assert_eq!(parse_version(""), None);
        assert!(parse_version("codex-cli 0.153.2").unwrap() >= MIN_VERSION);
        assert!(parse_version("codex-cli 0.152.9").unwrap() < MIN_VERSION);
    }
}
