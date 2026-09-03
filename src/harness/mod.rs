//! The agent harness: the CLI that pm launches inside a tmux window to run an
//! agent. Everything harness-specific — command shape, capability probes —
//! sits behind a `match` on [`Harness`] here, so the spawn chokepoint and
//! the registry stay harness-neutral.

mod claude_code;

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::PmError;

/// Which agent CLI a spawn runs on. Dispatched by `match` per seam rather
/// than a trait: the variant set is small and closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    #[default]
    ClaudeCode,
}

impl Harness {
    /// The string form used in config, the registry, and `pm agent list`.
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
        }
    }

    /// Every harness pm can spawn, in the order shown in error messages.
    pub const SUPPORTED: &[Harness] = &[Harness::ClaudeCode];

    /// The command line that launches an agent for `spec`.
    pub fn build_cmd(self, spec: &SpawnSpec<'_>) -> String {
        match self {
            Harness::ClaudeCode => claude_code::build_cmd(spec),
        }
    }

    /// Whether the installed harness binary supports appending a prompt
    /// file (how pm applies the shared baseline). `None` when the binary
    /// can't be probed at all.
    pub fn supports_prompt_file(self) -> Option<bool> {
        match self {
            Harness::ClaudeCode => claude_code::supports_append_file(),
        }
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Harness {
    type Err = PmError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Harness::SUPPORTED
            .iter()
            .copied()
            .find(|h| h.as_str() == s)
            .ok_or_else(|| PmError::HarnessUnsupported {
                value: s.to_string(),
                supported: Harness::SUPPORTED
                    .iter()
                    .map(|h| h.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
    }
}

/// A harness-neutral description of the agent session pm wants to launch.
/// Every field left unset omits the corresponding flag, so the agent
/// inherits the harness's own default for that setting.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpawnSpec<'a> {
    /// Agent definition to launch; `None` is a plain, definition-less session.
    pub definition: Option<&'a str>,
    /// The composed baseline + notice-board file appended to the system prompt.
    pub append_prompt_file: Option<&'a str>,
    /// Initial positional prompt (or the never-idle sentinel).
    pub prompt: Option<&'a str>,
    /// Session id to resume.
    pub resume_session: Option<&'a str>,
    /// With `resume_session`, load its transcript under a fresh session id.
    pub fork_session: bool,
    pub permission_mode: Option<&'a str>,
    pub model: Option<&'a str>,
}

/// A session id is bound to the harness that produced it. Returns the id
/// to resume only when `stored` (the harness recorded on the registry
/// entry) still matches `resolved` (what config says now); otherwise the
/// agent must start fresh.
pub fn resumable_session(session_id: &str, stored: Harness, resolved: Harness) -> Option<String> {
    if session_id.is_empty() || stored != resolved {
        return None;
    }
    Some(session_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_supported_names() {
        assert_eq!(
            "claude-code".parse::<Harness>().unwrap(),
            Harness::ClaudeCode
        );
        assert_eq!(Harness::ClaudeCode.to_string(), "claude-code");
        assert_eq!(Harness::default(), Harness::ClaudeCode);
    }

    #[test]
    fn unsupported_name_errors_with_supported_list() {
        let err = "codex".parse::<Harness>().unwrap_err().to_string();
        assert_eq!(
            err,
            "harness 'codex' is not supported yet; supported: claude-code"
        );
    }

    #[test]
    fn serde_uses_kebab_case_string() {
        #[derive(Serialize, Deserialize)]
        struct Wrap {
            h: Harness,
        }
        let toml_str = toml::to_string(&Wrap {
            h: Harness::ClaudeCode,
        })
        .unwrap();
        assert_eq!(toml_str.trim(), r#"h = "claude-code""#);
        let back: Wrap = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.h, Harness::ClaudeCode);
    }

    #[test]
    fn claude_code_build_cmd_full_spec() {
        // Every field set at once, pinning flag order through the seam.
        let cmd = Harness::ClaudeCode.build_cmd(&SpawnSpec {
            definition: Some("reviewer"),
            append_prompt_file: Some("/proj/main/.claude/pm-baseline.md"),
            prompt: Some("Stand by."),
            resume_session: Some("abc123"),
            fork_session: true,
            permission_mode: Some("acceptEdits"),
            model: Some("opus"),
        });
        assert_eq!(
            cmd,
            "claude --agent reviewer --model 'opus' \
             --append-system-prompt-file '/proj/main/.claude/pm-baseline.md' \
             --permission-mode 'acceptEdits' --resume abc123 --fork-session 'Stand by.'"
        );
    }

    #[test]
    fn resumable_session_needs_an_id() {
        assert_eq!(
            resumable_session("sess", Harness::ClaudeCode, Harness::ClaudeCode).as_deref(),
            Some("sess")
        );
        assert_eq!(
            resumable_session("", Harness::ClaudeCode, Harness::ClaudeCode),
            None
        );
    }
}
