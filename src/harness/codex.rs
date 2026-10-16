//! Codex: the window runs `pm harness codex-host` ([`host`]), which starts
//! the agent's own `codex app-server` and runs the TUI against it
//! (`codex --remote unix://<socket>`). Approval `never` is the default because
//! an approval prompt in an unwatched tmux window stalls the agent.
//!
//! The server is the agent's alone: hooks execute in the server, with its
//! environment, which is the window's. A TUI attached to the per-user
//! app-server daemon (started on demand since 0.157) would run hooks in
//! the daemon, whose environment is that of whichever client started it:
//! `PM_AGENT_NAME` absent or another agent's, so pm's guarded hooks exit
//! silently and the agent idles after its first turn.
//!
//! pm prompts the agent through its server ([`app_server`]): `turn/start`
//! starts a turn on an idle thread, on one Esc interrupted, and on one
//! not yet prompted, and steers into a running turn, delivered after its
//! next tool result. It works from inside codex's Interrupt hook, whose
//! timeout codex clamps to 3 s. A session reports the server's socket and
//! its thread as its inbound channel at SessionStart ([`inbound`]).
//!
//! Codex has no launch-time role channel (no `--agent`), so pm's composed
//! prompt — definition body, baseline, notice boards — is injected by the
//! SessionStart hook as `hookSpecificOutput.additionalContext`; SessionStart
//! also fires on `codex resume`, so the role re-applies without extra flags.
//! `hooks.json` is read at launch: a running session keeps the entries it
//! started with.
//! Hooks live in `$CODEX_HOME/hooks.json` in Claude Code's nested shape (a
//! flat shape parses and registers nothing). Codex reads skills from the
//! canonical `.agents/skills/` directly, so nothing is projected for it.
//!
//! UserPromptSubmit, like Claude Code's, runs for submitted input and not
//! for a Stop `block` continuation (from the 0.157 source: the continuation
//! is recorded without passing through `inspect_pending_input`; not
//! verified live). Its stdout becomes developer context.
//!
//! Two trust gates in `$CODEX_HOME/config.toml` stand between a spawn and a
//! working agent: `[projects."<dir>"] trust_level = "trusted"`, which pm writes
//! per worktree before launching, and `[hooks.state."<hooks.json>:<event>:<i>:<j>"]
//! trusted_hash`, a hash only codex computes, re-asked for when a hook
//! changes. The user grants it at codex's "Trust all and continue", or
//! consents once to pm having codex record it at every hook install
//! ([`hook_trust`]). Absent hook trust, hooks silently do not run.
//!
//! pm's tmux socket is unreachable from inside any codex sandbox (macOS
//! Seatbelt blocks `AF_UNIX` connect), so the default sandbox is
//! `danger-full-access` — the same blast radius as pm's Claude Code agents.
//! A sandboxed agent (`workspace-write` + writable roots, or `read-only` as
//! [`READ_ONLY_PROFILE`]) can still read, run git, and send messages; it
//! cannot spawn, heal, or stop agents.
//!
//! A resume or fork carries `-c tui.resume_cwd="current"`: when a session
//! was last run in another directory the TUI otherwise stops at an
//! interactive directory chooser, which stalls an unwatched window. The
//! session then continues in the window's directory, which a remote TUI
//! needs named with `-C`, canonical, or it asks to trust the directory.
//!
//! An agent kept out of a directory (`SpawnSpec::protected_dirs`) is kept
//! out by pm's guard hook ([`guard`]), not at launch.
//!
//! The waiter starts only as a turn ends, so a spawn launches codex with a
//! prompt (`agent_spawn`'s `SPAWN_PROMPT`, or `RESUME_PROMPT` on a resume),
//! which [`host`] gives with `turn/start`. SessionStart runs only as the
//! first turn starts, and an async SessionStart's exit 2 is ignored, so
//! no hook arms a session that has had no turn.
//!
//! Codex's SessionEnd runs only as the server tears a root thread down,
//! which neither the TUI's exit nor `/new` does under [`host`]: the host
//! ends the server with SIGTERM, which skips it (verified on 0.162.1). So
//! pm installs no session-end hook for codex, whose window reads dead once
//! the host has stopped the server, within 2 s.

pub(super) mod app_server;
pub(super) mod chat;
pub(super) mod dialog;
pub(super) mod guard;
pub(super) mod hook_trust;
pub(super) mod host;
pub(super) mod input;
pub(super) mod mode;
pub(super) mod sessions;
pub(super) mod transcript;
pub(super) mod waiting;

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::harness::probe::{self, Probe, parse_version};
use crate::harness::{SpawnSpec, Wake};
use crate::state::project::CodexConfig;
use crate::state::runtime::{Endpoint, Inbound};
use crate::tmux;

pub(super) const CONFIG_DIR: &str = ".codex";

/// The program an agent's window runs.
pub(super) const BINARY: &str = "codex";

/// codex asks the macOS keychain as it starts: with it hung, even
/// `codex exec` hangs.
pub(super) const READS_KEYCHAIN: bool = true;
pub(super) const HOOKS_FILE: &str = "hooks.json";
const CONFIG_FILE: &str = "config.toml";

/// The release whose app-server and remote TUI pm's launch was verified
/// on.
pub(super) const MIN_VERSION: (u32, u32, u32) = (0, 160, 0);

/// The sandbox mode whose writable roots pm opens up.
const WORKSPACE_WRITE: &str = "workspace-write";
/// Without the writable roots `pm msg read` and `send` fail, and this mode
/// has none, so pm swaps it for [`READ_ONLY_PROFILE`].
const READ_ONLY: &str = "read-only";
/// The permission profile pm defines on the server for a `read-only`
/// agent: the whole disk readable, only the writable roots writable.
const READ_ONLY_PROFILE: &str = "pm-read-only";
const DEFAULT_SANDBOX: &str = "danger-full-access";
const DEFAULT_APPROVAL: &str = "never";

const RESUME_IN_CURRENT_DIR: &str = "tui.resume_cwd=\"current\"";

/// pm's Stop hook is `async`: codex runs it in the background once the
/// turn has ended, and it outlives the turn and Esc. Its continuation is
/// posted to the session's server ([`post`]).
pub(super) const STOP_HOOK_OPTIONS: &[(&str, bool)] = &[("async", true)];

/// See the module docs.
pub(super) const NEEDS_LAUNCH_PROMPT: bool = true;

/// The inbound channel of `agent`'s session `thread`: its server's socket,
/// once the server listens there. A session launched without
/// [`host`] has none.
pub(super) fn inbound(
    project_root: &Path,
    scope: &str,
    agent: &str,
    thread: &str,
) -> Option<Inbound> {
    let socket = host::socket_path(project_root, scope, agent).ok()?;
    std::fs::symlink_metadata(&socket).ok()?;
    Some(Inbound {
        endpoint: Endpoint::Socket(socket),
        token: None,
        thread: Some(thread.to_string()).filter(|t| !t.is_empty()),
    })
}

/// Start a turn of the session on `inbound` with `text`, or steer it into
/// the turn running.
pub(super) fn post(inbound: &Inbound, text: &str) -> Result<()> {
    let thread = inbound.thread.as_deref().ok_or_else(|| {
        PmError::Agent("its codex session reported no thread to prompt".to_string())
    })?;
    let socket = inbound.socket().ok_or_else(|| {
        PmError::Agent("its codex session reported no app-server socket".to_string())
    })?;
    app_server::Client::within(socket, POST_WITHIN)?.start_turn(thread, text, None)
}

/// How long a post may take, connecting included: codex kills the
/// Interrupt hook, which may post, 3 s after it starts.
const POST_WITHIN: std::time::Duration = std::time::Duration::from_millis(1500);

pub(super) const TRUST_CONSENT_KEY: &str = "harness.codex.trust_pm_hooks";

pub(super) const RETIRED_CONFIG_KEYS: &[(&str, &str)] = &[(
    "harness.codex.bypass_hook_trust",
    "to have pm trust its own hooks in codex instead, run `pm config set --global \
     harness.codex.trust_pm_hooks true`, then `pm harness hooks install`",
)];

/// How a user grants codex's trust in pm's hooks without pm.
pub(super) const MANUAL_TRUST: &str = "or start `codex` once in a trusted directory and choose \
    \"Trust all and continue\"";

/// How a user has pm trust its hooks, by whether they consented to it.
pub(super) fn hook_trust_remedy(consented: bool) -> String {
    if consented {
        format!("run `pm harness hooks install` ({MANUAL_TRUST})")
    } else {
        format!(
            "run `pm config set --global {TRUST_CONSENT_KEY} true`, then `pm harness hooks \
             install` ({MANUAL_TRUST})"
        )
    }
}

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

/// The sandbox mode an agent with `permission_mode` runs in.
fn sandbox_mode<'a>(cfg: &'a CodexConfig, permission_mode: Option<&'a str>) -> &'a str {
    permission_mode
        .or(cfg.sandbox.as_deref())
        .unwrap_or(DEFAULT_SANDBOX)
}

/// The window's command: `pm harness codex-host` with the settings
/// [`host`] launches the server and TUI with.
pub(super) fn build_cmd(spec: &SpawnSpec<'_>, cfg: &CodexConfig) -> String {
    let SpawnSpec {
        definition: _,
        session_name,
        append_prompt_file: _,
        prompt,
        resume_session,
        fork_session,
        permission_mode,
        model,
        writable_dirs,
        edit_dirs: _,
        protected_dirs: _,
    } = *spec;

    let approval = cfg.approval.as_deref().unwrap_or(DEFAULT_APPROVAL);
    // The per-agent permission row is the sandbox mode; `[harness.codex]`
    // supplies the harness-wide default.
    let sandbox = sandbox_mode(cfg, permission_mode);

    let mut parts: Vec<String> = ["pm", "harness", "codex-host", "--approval"]
        .map(str::to_string)
        .to_vec();
    parts.push(tmux::shell_quote(approval));
    parts.push("--sandbox".to_string());
    parts.push(tmux::shell_quote(sandbox));
    if sandbox == WORKSPACE_WRITE || sandbox == READ_ONLY {
        for dir in writable_dirs {
            parts.push("--writable".to_string());
            parts.push(tmux::shell_quote(&dir.to_string_lossy()));
        }
    }
    if let Some(id) = model {
        parts.push("--model".to_string());
        parts.push(tmux::shell_quote(id));
    }
    if let Some(session_id) = resume_session {
        parts.push(if fork_session { "--fork" } else { "--resume" }.to_string());
        parts.push(tmux::shell_quote(session_id));
    }
    if let Some(p) = prompt {
        parts.push("--prompt".to_string());
        parts.push(tmux::shell_quote(p));
    }
    if let Some(name) = session_name {
        parts.push("--name".to_string());
        parts.push(tmux::shell_quote(name));
    }
    parts.join(" ")
}

/// The `-c` overrides that define [`READ_ONLY_PROFILE`] and select it.
fn read_only_profile(writable_dirs: &[PathBuf]) -> [String; 2] {
    let mut filesystem = toml_edit::InlineTable::new();
    filesystem.insert(":root", "read".into());
    for dir in writable_dirs {
        filesystem.insert(dir.to_string_lossy(), "write".into());
    }
    [
        format!("default_permissions=\"{READ_ONLY_PROFILE}\""),
        format!("permissions.{READ_ONLY_PROFILE}.filesystem={filesystem}"),
    ]
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

/// Whether codex has a trust entry for `hooks.state.<key>`, current or
/// not.
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

/// The installed version's raw string, or `None` when `codex` can't be run.
pub(super) fn installed_version(probe: Probe) -> Option<String> {
    let exit = probe::run(BINARY, BINARY, "--version", probe).ok()?;
    exit.success.then(|| exit.stdout.trim().to_string())
}

/// Whether the installed codex is at least [`MIN_VERSION`]; `None` when it
/// can't be probed.
pub(super) fn version_supported(probe: Probe) -> Option<bool> {
    Some(parse_version(&installed_version(probe)?)? >= MIN_VERSION)
}

pub(super) fn unusable_reason(probe: Probe) -> Option<String> {
    version_problem(installed_version(probe).as_deref())
}

/// What is wrong with `found`, the installed version's raw string (`None`
/// when codex can't be run).
fn version_problem(found: Option<&str>) -> Option<String> {
    let Some(found) = found else {
        return Some("`codex` could not be run; install codex".to_string());
    };
    match parse_version(found) {
        Some(version) if version >= MIN_VERSION => None,
        _ => Some(format!(
            "installed codex is `{found}`; pm runs each agent on an app-server of its own, \
             which needs {} or later",
            min_version_string()
        )),
    }
}

/// Exits 0 only when codex holds credentials.
pub(super) const LOGIN_STATUS: &[&str] = &["login", "status"];

/// The manual step that gives a new machine's codex its credentials,
/// which live in `$CODEX_HOME/auth.json`.
pub(super) const CREDENTIALS_STEP: &str = "log in to codex (`codex login`)";
pub(super) const EXPORT_TAG: &str = "codex";
pub(super) const WAKE: Wake = Wake::Post;

pub(super) fn prompt_mechanism() -> String {
    format!(
        "SessionStart hook context injection on the agent's own app-server \
         (needs codex >= {})",
        min_version_string()
    )
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
    fn build_cmd_runs_the_host_with_what_config_resolved() {
        let dirs = vec![PathBuf::from("/proj/.pm"), PathBuf::from("/proj/main/.git")];
        // The definition and prompt file are delivered by the SessionStart
        // hook, never on the command line.
        let cmd = build_cmd(
            &SpawnSpec {
                definition: Some("reviewer"),
                append_prompt_file: Some("/x/baseline.md"),
                prompt: Some("Stand by."),
                session_name: Some("reviewer.login"),
                writable_dirs: &dirs,
                ..Default::default()
            },
            &cfg(),
        );
        assert_eq!(
            cmd,
            "pm harness codex-host --approval 'never' --sandbox 'danger-full-access' \
             --prompt 'Stand by.' --name 'reviewer.login'"
        );

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
                resume_session: Some("abc"),
                fork_session: true,
                ..Default::default()
            },
            &config,
        );
        assert_eq!(
            cmd,
            "pm harness codex-host --approval 'on-request' --sandbox 'workspace-write' \
             --writable '/proj/.pm' --writable '/proj/main/.git' --model 'gpt-5' \
             --fork 'abc'"
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
    fn version_problem_names_a_missing_or_old_binary() {
        assert_eq!(
            version_problem(None).unwrap(),
            "`codex` could not be run; install codex"
        );
        assert_eq!(
            version_problem(Some("codex-cli 0.155.9")).unwrap(),
            "installed codex is `codex-cli 0.155.9`; pm runs each agent on an app-server of its \
             own, which needs 0.160.0 or later"
        );
        assert_eq!(version_problem(Some("codex-cli 0.160.0")), None);
    }
}
