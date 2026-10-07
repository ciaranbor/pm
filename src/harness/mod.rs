//! The agent harness: the CLI that pm launches inside a tmux window to run an
//! agent. Everything harness-specific — command shape, capability probes,
//! where it reads agent definitions and skills from, trust it needs before it
//! will run pm's hooks — sits behind a `match` on [`Harness`], so the spawn
//! chokepoint, the asset installer, the hooks, and the registry stay
//! harness-neutral. Per-agent settings that only the harness can interpret
//! (model id, permission mode) are passed through verbatim.
//!
//! Each seam is a method on [`Harness`] in a `seams/` file, grouped by
//! concern and matching exhaustively.

mod claude_code;
mod codex;
mod hook_trust;
mod opencode;
pub(crate) mod probe;
mod projection;
mod screen;
mod session_store;
mod spawn;
pub(crate) mod transcript;

mod seams {
    mod assets;
    pub(super) mod config;
    mod hooks;
    mod input;
    mod launch;
    pub(super) mod never_idle;
    mod sessions;
}

pub use hook_trust::{HookTrust, HookTrustStatus};
#[cfg(test)]
pub(crate) use opencode::chat::testing as opencode_testing;
pub use probe::Probe;
pub(crate) use projection::project_by_copy;
pub use projection::{Projection, ProjectionScope};
pub use seams::config::{ConfigIssue, ConfigIssueKind};
pub use seams::never_idle::{WaitingEvent, Wake};
pub use session_store::{
    AgentSession, Conversation, ExportJob, ImportOutcome, InUse, SessionStore,
};
pub(crate) use session_store::{per_session_outcome, session_counts};
pub use spawn::{LaunchContext, PreLaunch, SpawnSpec, resumable_session};

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::PmError;
use crate::state::project::{AgentsConfig, layered};

/// `text`'s first line, cut to a length that fits a status line: at its
/// end, or for a line without spaces (a path), at its start, since a
/// path's end names the file.
pub(crate) fn one_line(text: &str) -> String {
    const MAX: usize = 120;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let chars = line.chars().count();
    if chars <= MAX {
        return line.to_string();
    }
    if line.contains(char::is_whitespace) {
        let (cut, _) = line.char_indices().nth(MAX).expect("longer than MAX");
        format!("{}…", &line[..cut])
    } else {
        let (cut, _) = line
            .char_indices()
            .nth(chars - MAX)
            .expect("longer than MAX");
        format!("…{}", &line[cut..])
    }
}

/// Which agent CLI a spawn runs on. Dispatched by `match` per seam rather
/// than a trait: the variant set is small and closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    #[default]
    ClaudeCode,
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
}

impl Harness {
    /// The string form used in config, the registry, and `pm agent list`.
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
        }
    }

    /// Every harness pm can spawn, in the order shown in error messages.
    pub const SUPPORTED: &[Harness] = &[Harness::ClaudeCode, Harness::Codex, Harness::OpenCode];
}

/// The longest pm waits on one call to a harness binary that answers from
/// local state, killing it after.
pub(crate) const CALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Every harness the project's agents run on: the default plus whatever
/// `[agents.harness]` resolves to for any configured agent, with the same
/// project-over-global, `""`-masks precedence as a spawn, so pm stops
/// installing for a masked harness. Unparseable values are skipped here —
/// they error at spawn, where it matters.
pub fn harnesses_in_use(project: &AgentsConfig, global: &AgentsConfig) -> Vec<Harness> {
    let mut out = vec![Harness::default()];
    for key in project.harness.keys().chain(global.harness.keys()) {
        if let Some(value) = layered(&project.harness, &global.harness, key)
            && let Ok(h) = value.parse::<Harness>()
            && !out.contains(&h)
        {
            out.push(h);
        }
    }
    out
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Harness {
    type Err = PmError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_line_is_cut_at_its_end_and_a_long_path_at_its_start() {
        let words = format!("{} end", "word ".repeat(40));
        let cut = one_line(&words);
        assert!(cut.starts_with("word word") && cut.ends_with('…'), "{cut}");
        assert_eq!(cut.chars().count(), 121);

        let path = format!("/tmp/{}/important_file_name.txt", "deep/".repeat(40));
        let cut = one_line(&path);
        assert!(
            cut.starts_with('…') && cut.ends_with("/important_file_name.txt"),
            "{cut}"
        );
        assert_eq!(cut.chars().count(), 121);
    }

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
        let err = "aider".parse::<Harness>().unwrap_err().to_string();
        assert_eq!(
            err,
            "harness 'aider' is not supported yet; supported: claude-code, codex, opencode"
        );
        assert_eq!("codex".parse::<Harness>().unwrap(), Harness::Codex);
        assert_eq!(Harness::Codex.to_string(), "codex");
        assert_eq!("opencode".parse::<Harness>().unwrap(), Harness::OpenCode);
        assert_eq!(Harness::OpenCode.to_string(), "opencode");
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
        // The registry's spelling is the config's, for every harness.
        for h in Harness::SUPPORTED {
            let stored = toml::to_string(&Wrap { h: *h }).unwrap();
            assert_eq!(stored.trim(), format!("h = \"{h}\""));
            assert_eq!(toml::from_str::<Wrap>(&stored).unwrap().h, *h);
        }
    }

    #[test]
    fn harnesses_in_use_is_default_plus_configured() {
        let mut project = AgentsConfig::default();
        project
            .harness
            .insert("implementer".into(), "claude-code".into());
        project.harness.insert("x".into(), "aider".into());
        assert_eq!(
            harnesses_in_use(&project, &AgentsConfig::default()),
            vec![Harness::ClaudeCode]
        );
        project.harness.insert("main".into(), "codex".into());
        assert_eq!(
            harnesses_in_use(&project, &AgentsConfig::default()),
            vec![Harness::ClaudeCode, Harness::Codex]
        );
    }

    #[test]
    fn harnesses_in_use_counts_a_wildcard_row() {
        let mut global = AgentsConfig::default();
        global.harness.insert("*".into(), "codex".into());
        assert_eq!(
            harnesses_in_use(&AgentsConfig::default(), &global),
            vec![Harness::ClaudeCode, Harness::Codex]
        );
        // A project wildcard masks the global one for every agent.
        let mut project = AgentsConfig::default();
        project.harness.insert("*".into(), String::new());
        assert_eq!(
            harnesses_in_use(&project, &global),
            vec![Harness::ClaudeCode]
        );
    }

    #[test]
    fn harnesses_in_use_applies_project_over_global_precedence() {
        let mut global = AgentsConfig::default();
        global.harness.insert("main".into(), "codex".into());
        assert_eq!(
            harnesses_in_use(&AgentsConfig::default(), &global),
            vec![Harness::ClaudeCode, Harness::Codex]
        );

        // The project masks the global row: codex is no longer in use.
        let mut project = AgentsConfig::default();
        project.harness.insert("main".into(), String::new());
        assert_eq!(
            harnesses_in_use(&project, &global),
            vec![Harness::ClaudeCode]
        );
        // …or overrides it outright.
        project.harness.insert("main".into(), "claude-code".into());
        assert_eq!(
            harnesses_in_use(&project, &global),
            vec![Harness::ClaudeCode]
        );
        // A global row for another agent still counts.
        global.harness.insert("reviewer".into(), "codex".into());
        assert_eq!(
            harnesses_in_use(&project, &global),
            vec![Harness::ClaudeCode, Harness::Codex]
        );
    }
}
