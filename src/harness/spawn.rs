//! The harness-neutral description of a launch.

use std::path::{Path, PathBuf};

use super::Harness;

/// A harness-neutral description of the agent session pm wants to launch.
/// Every field left unset omits the corresponding flag, so the agent
/// inherits the harness's own default for that setting.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpawnSpec<'a> {
    /// Agent definition to launch; `None` is a plain, definition-less session.
    pub definition: Option<&'a str>,
    /// What the harness shows the session as ([`session_name`]).
    pub session_name: Option<&'a str>,
    /// The composed baseline + notice-board file appended to the system prompt.
    pub append_prompt_file: Option<&'a str>,
    /// Initial positional prompt.
    pub prompt: Option<&'a str>,
    /// Session id to resume.
    pub resume_session: Option<&'a str>,
    /// With `resume_session`, load its transcript under a fresh session id.
    pub fork_session: bool,
    /// Harness-specific, like `model`: whatever the config row said.
    pub permission_mode: Option<&'a str>,
    pub model: Option<&'a str>,
    /// Directories outside the worktree the agent must be able to write
    /// (pm's state, the shared `.git`) — what a sandboxing harness opens up.
    pub writable_dirs: &'a [PathBuf],
    /// Directories outside the worktree the agent edits with its own file
    /// tools (the feature summary's), for a harness that gates those edits
    /// by directory. Each lies within a `writable_dirs` entry.
    pub edit_dirs: &'a [PathBuf],
    /// Directories the agent must not edit (main's checkout, for an agent
    /// of another scope). Each harness enforces this its own way; see its
    /// module docs.
    pub protected_dirs: &'a [PathBuf],
}

/// The name a harness shows an agent's session under, where it names
/// sessions: the agent and its scope, so a session list tells pm's agents
/// apart.
pub fn session_name(agent: &str, scope: &str) -> String {
    format!("{agent}.{scope}")
}

/// `dir` as a glob pattern's literal prefix: `None` when it holds a
/// character a harness's path glob would read as a metacharacter, so the
/// harness cannot express a rule for it.
pub(super) fn glob_literal(dir: &Path) -> Option<String> {
    let text = dir.to_str()?;
    (!text.contains(['*', '?', '[', ']', '{', '}', '\\'])).then(|| text.to_string())
}

/// The spawn note for the protected dirs that [`glob_literal`] cannot
/// name, for a harness whose rules are globs.
pub(super) fn unguarded_note(protected_dirs: &[PathBuf]) -> Option<String> {
    let names: Vec<String> = protected_dirs
        .iter()
        .filter(|dir| glob_literal(dir).is_none())
        .map(|dir| dir.display().to_string())
        .collect();
    (!names.is_empty()).then(|| {
        format!(
            "not guarding {} against edits: a permission rule cannot name its path",
            names.join(", ")
        )
    })
}

/// Where a spawn happens, for [`Harness::pre_launch`].
#[derive(Debug, Clone, Copy)]
pub struct LaunchContext<'a> {
    pub project_root: &'a Path,
    /// The scope the agent belongs to.
    pub feature: &'a str,
    pub worktree: &'a Path,
    /// The agent's display name.
    pub agent: &'a str,
}

/// What [`Harness::pre_launch`] prepared for the command line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PreLaunch {
    /// The session the harness will open, known before it starts — recorded
    /// on the registry entry at spawn. `None` for a harness that reports its
    /// session through the SessionStart hook.
    pub session_id: Option<String>,
    /// Environment the command must run with.
    pub env: Vec<(String, String)>,
    /// Variables the command must not inherit from the window's shell.
    pub env_remove: Vec<String>,
    /// Remarks for the spawn line.
    pub notes: Vec<String>,
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
    use crate::state::project::HarnessConfig;

    #[test]
    fn claude_code_build_cmd_full_spec() {
        // Every field set at once, pinning flag order through the seam.
        let dirs = vec![PathBuf::from("/proj/.pm")];
        let edit = vec![PathBuf::from("/proj/.pm/summaries")];
        let protected = vec![PathBuf::from("/proj/main")];
        let cmd = Harness::ClaudeCode.build_cmd(
            &SpawnSpec {
                definition: Some("reviewer"),
                session_name: Some("reviewer.login"),
                append_prompt_file: Some("/proj/main/.agents/pm-baseline.md"),
                prompt: Some("go"),
                resume_session: Some("abc123"),
                fork_session: true,
                permission_mode: Some("acceptEdits"),
                model: Some("opus"),
                writable_dirs: &dirs,
                edit_dirs: &edit,
                protected_dirs: &protected,
            },
            &HarnessConfig::default(),
            &PreLaunch::default(),
        );
        assert_eq!(
            cmd,
            "claude --agent reviewer -n 'reviewer.login' --model 'opus' \
             --append-system-prompt-file '/proj/main/.agents/pm-baseline.md' \
             --permission-mode 'acceptEdits' --add-dir='/proj/.pm/summaries' \
             --settings '{\"permissions\":{\"deny\":[\"Edit(//proj/main/**)\"]}}' \
             --resume abc123 --fork-session 'go'"
        );
    }

    #[test]
    fn resumable_session_needs_an_id_and_the_same_harness() {
        assert_eq!(
            resumable_session("sess", Harness::ClaudeCode, Harness::ClaudeCode).as_deref(),
            Some("sess")
        );
        assert_eq!(
            resumable_session("sess", Harness::Codex, Harness::Codex).as_deref(),
            Some("sess")
        );
        assert_eq!(
            resumable_session("", Harness::ClaudeCode, Harness::ClaudeCode),
            None
        );
        assert_eq!(
            resumable_session("sess", Harness::ClaudeCode, Harness::Codex),
            None
        );
    }
}
