//! Claude Code: `claude --agent <def> …`.

use crate::harness::SpawnSpec;
use crate::tmux;

pub(super) fn build_cmd(spec: &SpawnSpec<'_>) -> String {
    let SpawnSpec {
        definition,
        append_prompt_file,
        prompt,
        resume_session,
        fork_session,
        permission_mode,
        model,
    } = *spec;

    let mut parts = vec!["claude".to_string()];

    if let Some(name) = definition {
        parts.push("--agent".to_string());
        parts.push(name.to_string());
    }

    // Config-sourced values are shell-quoted: the command is sent through the
    // user's interactive shell, and a model id may carry a bracketed context
    // suffix (e.g. `claude-opus-4-8[1m]`) that zsh would try to glob.
    if let Some(id) = model {
        parts.push("--model".to_string());
        parts.push(tmux::shell_quote(id));
    }

    if let Some(file) = append_prompt_file {
        parts.push("--append-system-prompt-file".to_string());
        parts.push(tmux::shell_quote(file));
    }

    if let Some(mode) = permission_mode {
        parts.push("--permission-mode".to_string());
        parts.push(tmux::shell_quote(mode));
    }

    if let Some(session_id) = resume_session {
        parts.push("--resume".to_string());
        parts.push(session_id.to_string());
        // `--fork-session` only makes sense alongside `--resume`. It tells
        // Claude to load the source's transcript but assign a fresh
        // session id, so the fork's appends don't pollute the source.
        if fork_session {
            parts.push("--fork-session".to_string());
        }
    }

    if let Some(p) = prompt {
        parts.push(tmux::shell_quote(p));
    }

    parts.join(" ")
}

/// Whether `claude --help` text advertises `--append-system-prompt-file`,
/// the flag pm relies on to apply the shared agent baseline. Split out as a
/// pure function so it can be unit-tested without invoking `claude`.
///
/// `claude --help` collapses the pair into `--append-system-prompt[-file]`
/// rather than spelling out the `-file` variant, so match either that
/// bracketed form or a fully expanded `--append-system-prompt-file`. Both
/// disappear if the file variant is ever removed — which is what we want to
/// catch.
fn help_lists_append_file(help: &str) -> bool {
    help.contains("--append-system-prompt-file") || help.contains("--append-system-prompt[-file]")
}

/// Probe the installed `claude` for `--append-system-prompt-file` support.
///
/// - `Some(true)`  — claude ran and advertises the flag (expected).
/// - `Some(false)` — claude ran but does NOT advertise it. The baseline
///   mechanism has regressed: spawned agents would silently lose it.
/// - `None`        — `claude` not found or `--help` failed; nothing to spawn
///   against anyway, so callers treat this as "can't tell, don't warn".
pub(super) fn supports_append_file() -> Option<bool> {
    let out = std::process::Command::new("claude")
        .arg("--help")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let help = String::from_utf8_lossy(&out.stdout);
    Some(help_lists_append_file(&help))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cmd_with_agent() {
        // The never-idle sentinel is injected by the spawn chokepoint;
        // build_cmd itself is prompt-agnostic.
        let cmd = build_cmd(&SpawnSpec {
            definition: Some("reviewer"),
            ..Default::default()
        });
        assert_eq!(cmd, "claude --agent reviewer");
    }

    #[test]
    fn build_cmd_plain_session() {
        let cmd = build_cmd(&SpawnSpec::default());
        assert_eq!(cmd, "claude");
    }

    #[test]
    fn build_cmd_plain_session_with_permission() {
        let cmd = build_cmd(&SpawnSpec {
            permission_mode: Some("acceptEdits"),
            ..Default::default()
        });
        assert_eq!(cmd, "claude --permission-mode 'acceptEdits'");
    }

    #[test]
    fn build_cmd_with_context() {
        let cmd = build_cmd(&SpawnSpec {
            definition: Some("reviewer"),
            prompt: Some("review the auth module"),
            ..Default::default()
        });
        assert_eq!(cmd, "claude --agent reviewer 'review the auth module'");
    }

    #[test]
    fn build_cmd_with_resume() {
        let cmd = build_cmd(&SpawnSpec {
            definition: Some("reviewer"),
            resume_session: Some("abc123"),
            ..Default::default()
        });
        assert_eq!(cmd, "claude --agent reviewer --resume abc123");
    }

    #[test]
    fn build_cmd_with_context_and_resume() {
        let cmd = build_cmd(&SpawnSpec {
            definition: Some("reviewer"),
            prompt: Some("continue review"),
            resume_session: Some("abc123"),
            ..Default::default()
        });
        assert_eq!(
            cmd,
            "claude --agent reviewer --resume abc123 'continue review'"
        );
    }

    #[test]
    fn build_cmd_with_permission_mode() {
        let cmd = build_cmd(&SpawnSpec {
            definition: Some("implementer"),
            permission_mode: Some("acceptEdits"),
            ..Default::default()
        });
        assert_eq!(
            cmd,
            "claude --agent implementer --permission-mode 'acceptEdits'"
        );
    }

    #[test]
    fn build_cmd_with_append_prompt_file() {
        // The baseline path is appended right after `--agent` and is
        // shell-quoted so paths with spaces survive.
        let cmd = build_cmd(&SpawnSpec {
            definition: Some("reviewer"),
            append_prompt_file: Some("/proj/main/.claude/pm-baseline.md"),
            ..Default::default()
        });
        assert_eq!(
            cmd,
            "claude --agent reviewer --append-system-prompt-file '/proj/main/.claude/pm-baseline.md'"
        );
    }

    #[test]
    fn build_cmd_with_fork_session() {
        // `--fork-session` only emits when paired with `--resume`.
        let cmd = build_cmd(&SpawnSpec {
            definition: Some("reviewer"),
            resume_session: Some("abc123"),
            fork_session: true,
            ..Default::default()
        });
        assert_eq!(
            cmd,
            "claude --agent reviewer --resume abc123 --fork-session"
        );
    }

    #[test]
    fn build_cmd_fork_session_without_resume_is_noop() {
        // `--fork-session` requires `--resume` per claude's CLI; we drop it
        // silently rather than emit a broken command.
        let cmd = build_cmd(&SpawnSpec {
            definition: Some("reviewer"),
            fork_session: true,
            ..Default::default()
        });
        assert_eq!(cmd, "claude --agent reviewer");
    }

    #[test]
    fn build_cmd_with_model() {
        // Quoted, so a bracketed context suffix survives the interactive shell.
        let cmd = build_cmd(&SpawnSpec {
            definition: Some("reviewer"),
            model: Some("claude-opus-4-8[1m]"),
            ..Default::default()
        });
        assert_eq!(cmd, "claude --agent reviewer --model 'claude-opus-4-8[1m]'");
    }

    #[test]
    fn help_lists_append_file_detects_flag() {
        // Fully-expanded form.
        assert!(help_lists_append_file(
            "  --append-system-prompt-file <file>  Append a system prompt from a file\n"
        ));
        // The bracket-collapsed form `claude --help` actually emits today.
        assert!(help_lists_append_file(
            "                                        --append-system-prompt[-file], --add-dir\n"
        ));
        // Regressed: only the plain prompt variant remains, no `-file` — the
        // doctor capability check must flag this.
        assert!(!help_lists_append_file(
            "  --append-system-prompt <prompt>  Append a system prompt\n"
        ));
    }
}
