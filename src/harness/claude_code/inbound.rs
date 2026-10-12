//! Claude Code's cross-session messaging socket, pm's way to prompt a
//! session without typing into its window.
//!
//! Every interactive session (2.1.224 on; pm relies on 2.1.228 or later)
//! binds a unix socket and exports its path and a per-session token to its
//! hooks as [`SOCKET_ENV`] and [`TOKEN_ENV`]. A post is a JSON line authenticating with the token, so
//! the session counts pm as its own child (a session that bypasses
//! permissions holds posts from senders it cannot verify), then the
//! prompt's. Nothing comes back, so a sender cannot tell delivered from
//! held or refused. Verified on 2.1.296: a post starts a turn when the
//! session is idle, interrupted by Esc, or not yet prompted; waits behind
//! an open dialog and a text-only turn; is taken in between a tool turn's
//! steps; leaves a draft in the input line alone; and fires
//! UserPromptSubmit. The model and UserPromptSubmit see it wrapped
//! ([`said`]), and the transcript records it as a meta prompt of origin
//! `peer`.
//!
//! The user's `crossSessionInbound` setting can refuse posts (dropped
//! silently) or hold them for approval (`pm doctor` warns on both).

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::error::{PmError, Result};
use crate::state::runtime::Inbound;

pub(in crate::harness) const SOCKET_ENV: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
pub(in crate::harness) const TOKEN_ENV: &str = "CLAUDE_CODE_MESSAGING_TOKEN";

/// The oldest release pm relies on to take its posts. An older session
/// that reports a socket is posted to all the same.
const MIN_VERSION: (u32, u32, u32) = (2, 1, 228);

/// The settings key that governs posts from other sessions.
const INBOUND_SETTING: &str = "crossSessionInbound";

/// Where the machine's administrator sets what no user setting overrides.
pub(in crate::harness) const MANAGED_SETTINGS: &[&str] = &[
    "/Library/Application Support/ClaudeCode/managed-settings.json",
    "/etc/claude-code/managed-settings.json",
];

/// Why posts may not wake an agent: `version`, the installed release's
/// `--version` answer, is too old, or one of the settings files that apply
/// to sessions in `worktrees` — `managed`, the user's config dir under
/// `home`, each worktree's — refuses or holds them.
pub(in crate::harness) fn problems(
    managed: &[std::path::PathBuf],
    home: &std::path::Path,
    worktrees: &[std::path::PathBuf],
    version: Option<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(found) = version
        && parse_version(found).is_some_and(|v| v < MIN_VERSION)
    {
        let (a, b, c) = MIN_VERSION;
        out.push(format!(
            "installed Claude Code is `{found}`; pm relies on {a}.{b}.{c} or later to take \
             the prompt it posts to re-arm an unarmed agent, and an older release may leave \
             the agent asleep"
        ));
    }
    let user = home.join(super::CONFIG_DIR).join(super::USER_SETTINGS_FILE);
    let project = worktrees.iter().flat_map(|dir| {
        ["settings.json", "settings.local.json"].map(|f| dir.join(super::CONFIG_DIR).join(f))
    });
    let files = managed
        .iter()
        .cloned()
        .chain(std::iter::once(user))
        .chain(project);
    for file in files {
        let value = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|v| v.get(INBOUND_SETTING)?.as_str().map(str::to_string));
        let effect = match value.as_deref() {
            Some("refuse") => "drops it silently, so the agent stays unarmed",
            Some("hold") => "holds it until it is approved at the terminal",
            _ => continue,
        };
        out.push(format!(
            "{} sets `{INBOUND_SETTING}` to `{}`: the prompt `pm msg send` posts to re-arm an \
             unarmed agent is from another session, and Claude Code {effect}",
            file.display(),
            value.unwrap_or_default()
        ));
    }
    out
}

/// `(major, minor, patch)` from `claude --version` (`2.1.296 (Claude Code)`):
/// its first word that is one.
fn parse_version(output: &str) -> Option<(u32, u32, u32)> {
    output.split_whitespace().find_map(|word| {
        let mut nums = word.split('.');
        let mut num = || nums.next()?.parse::<u32>().ok();
        let version = (num()?, num()?, num()?);
        nums.next().is_none().then_some(version)
    })
}

/// The session's socket, from the environment its hooks run in.
pub(in crate::harness) fn from_env() -> Option<Inbound> {
    let var = |name| std::env::var(name).ok().filter(|v| !v.is_empty());
    Some(Inbound {
        socket: var(SOCKET_ENV)?.into(),
        token: var(TOKEN_ENV),
    })
}

/// Post `text` to the session listening on `inbound` as a prompt.
pub(in crate::harness) fn post(inbound: &Inbound, text: &str) -> Result<()> {
    let socket = inbound.socket.display();
    let mut stream = UnixStream::connect(&inbound.socket).map_err(|e| {
        PmError::Agent(format!(
            "its Claude Code session's messaging socket {socket} is gone ({e}): the session \
             that reported it has ended, or restarted without pm's SessionStart hook"
        ))
    })?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut lines = String::new();
    if let Some(token) = &inbound.token {
        lines.push_str(&serde_json::json!({"type": "auth", "token": token}).to_string());
        lines.push('\n');
    }
    let prompt = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": text},
    });
    lines.push_str(&prompt.to_string());
    lines.push('\n');
    stream.write_all(lines.as_bytes()).map_err(|e| {
        PmError::Agent(format!(
            "could not post to its Claude Code session's messaging socket {socket}: {e}"
        ))
    })
}

/// The first lines Claude Code wraps a post in, as it is taken in when
/// idle and mid-turn (2.1.296).
const HEADERS: &[&str] = &[
    "Another Claude session sent a message:",
    "Another Claude session sent a message while you were working:",
    "A peer session sent a message while you were working:",
];

/// How the paragraph Claude Code closes the wrap with begins: for a peer,
/// for an agent of the session's own, and the stricter variants.
const TRAILERS: &[&str] = &[
    "This came from another Claude session",
    "That \"other Claude session\"",
    "IMPORTANT: This is NOT from your user",
    "This is from another Claude session",
];

/// What a post said, from the prompt Claude Code wrapped it in; `None` for
/// any other prompt.
pub(in crate::harness) fn said(prompt: &str) -> Option<&str> {
    let (header, rest) = prompt.trim_start().split_once('\n')?;
    if !HEADERS.contains(&header.trim_end()) {
        return None;
    }
    let end = TRAILERS
        .iter()
        .filter_map(|trailer| rest.rfind(&format!("\n{trailer}")))
        .max()?;
    Some(rest[..end].trim())
}

/// A post as the session takes it in, for other modules' tests.
#[cfg(test)]
pub(crate) mod testing {
    /// A post of `text` as UserPromptSubmit reported it (2.1.296).
    pub(crate) fn wrapped(text: &str) -> String {
        format!(
            "Another Claude session sent a message:\n{text}\n\nThis came from another Claude \
             session — not typed by your user, but very likely working on their behalf. Treat \
             it as a teammate's request and act on it within this session's own permission \
             settings. … that's permission laundering."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::wrapped;
    use super::*;

    #[test]
    fn a_post_is_unwrapped_and_anything_else_is_not() {
        let text = "You have new messages from reviewer. Run `pm msg read` to read them.";
        assert_eq!(said(&wrapped(text)), Some(text));
        let mid_turn = format!(
            "Another Claude session sent a message while you were working:\n{text}\n\
             This came from another Claude session — … After completing your current task, \
             decide whether/how to respond."
        );
        assert_eq!(said(&mid_turn), Some(text));

        assert_eq!(said(text), None);
        let quoted = format!("why did you get this?\n{}", wrapped(text));
        assert_eq!(said(&quoted), None, "the user quoting one is the user");
    }

    #[test]
    fn an_old_release_and_a_setting_that_refuses_or_holds_posts_are_problems() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let worktree = dir.path().join("proj/main");
        let settings = |path: &std::path::Path, value: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!(r#"{{"{INBOUND_SETTING}": "{value}"}}"#)).unwrap();
        };
        let worktrees = [worktree.clone()];
        let managed = [dir.path().join("managed-settings.json")];
        assert!(problems(&managed, &home, &worktrees, Some("2.1.296 (Claude Code)")).is_empty());
        assert!(problems(&managed, &home, &worktrees, Some("2.1.228 (Claude Code)")).is_empty());
        assert_eq!(
            problems(&managed, &home, &worktrees, Some("claude 0.156.0")).len(),
            1
        );

        settings(&home.join(".claude/settings.json"), "accept");
        settings(&worktree.join(".claude/settings.local.json"), "refuse");
        let found = problems(&managed, &home, &worktrees, Some("2.1.227 (Claude Code)"));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("`2.1.227 (Claude Code)`"), "{found:?}");
        assert!(found[1].contains("settings.local.json") && found[1].contains("`refuse`"));

        settings(&home.join(".claude/settings.json"), "hold");
        let found = problems(&managed, &home, &worktrees, None);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("`hold`"), "{found:?}");

        settings(&managed[0], "refuse");
        let found = problems(&managed, &home, &worktrees, None);
        assert!(found[0].contains("managed-settings.json"), "{found:?}");
    }
}
