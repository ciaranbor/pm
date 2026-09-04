//! The agent harness: the CLI that pm launches inside a tmux window to run an
//! agent. Everything harness-specific — command shape, capability probes,
//! where it reads agent definitions and skills from — sits behind a `match`
//! on [`Harness`] here, so the spawn chokepoint, the asset installer, and the
//! registry stay harness-neutral. Per-agent settings that only the harness
//! can interpret (model id, permission mode) are passed through verbatim.

mod claude_code;

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{PmError, Result};
use crate::fs_utils;
use crate::state::project::AgentsConfig;

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

    /// The harness's own config directory, relative to a worktree or home
    /// (`.claude` for claude-code). Agent definitions and skills are
    /// projected from the canonical `.agents/` store into it.
    pub fn config_dir(self) -> &'static str {
        match self {
            Harness::ClaudeCode => claude_code::CONFIG_DIR,
        }
    }

    /// Per-worktree files under [`config_dir`](Self::config_dir) that a
    /// feature worktree needs a copy of from main.
    pub fn seeded_files(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => claude_code::SEEDED_FILES,
        }
    }

    /// Subdirectories of the canonical store this harness projects into
    /// [`config_dir`](Self::config_dir) — and so the ones a feature
    /// worktree needs seeded from both.
    pub fn projected_dirs(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => claude_code::PROJECTED_DIRS,
        }
    }

    /// Project the canonical asset store (`<canonical_root>/{agents,skills}`)
    /// into this harness's own layout under `target_root`. Copies over
    /// same-named files and never deletes anything in the target. In
    /// `dry_run` nothing is written; the report still says what would be.
    pub fn project_assets(
        self,
        canonical_root: &Path,
        target_root: &Path,
        dry_run: bool,
    ) -> Result<Projection> {
        match self {
            Harness::ClaudeCode => {
                claude_code::project_assets(canonical_root, target_root, dry_run)
            }
        }
    }
}

/// What a projection wrote (or, in dry-run, would write), as paths
/// relative to the target root.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Projection {
    pub written: Vec<PathBuf>,
    /// The subset of `written` that replaced an existing file with
    /// different content.
    pub replaced: Vec<PathBuf>,
}

impl Projection {
    pub fn is_empty(&self) -> bool {
        self.written.is_empty()
    }
}

/// Copy `<canonical_root>/<subdir>` over `<target_root>/<subdir>` for each
/// `subdirs` entry, recording what changed. Shared by every harness whose
/// projection is a plain copy.
pub(crate) fn project_by_copy(
    canonical_root: &Path,
    target_root: &Path,
    subdirs: &[&str],
    dry_run: bool,
) -> Result<Projection> {
    let mut out = Projection::default();
    for sub in subdirs {
        let src = canonical_root.join(sub);
        if !src.is_dir() {
            continue;
        }
        let dst = target_root.join(sub);
        for (rel, replaced) in fs_utils::sync_tree(&src, &dst, dry_run)? {
            let rel = Path::new(sub).join(rel);
            if replaced {
                out.replaced.push(rel.clone());
            }
            out.written.push(rel);
        }
    }
    Ok(out)
}

/// Every harness the project's agents run on: the default plus whatever
/// `[agents.harness]` names in either config tier. Unparseable values are
/// skipped here — they error at spawn, where it matters.
pub fn harnesses_in_use(project: &AgentsConfig, global: &AgentsConfig) -> Vec<Harness> {
    let mut out = vec![Harness::default()];
    for value in project.harness.values().chain(global.harness.values()) {
        if let Ok(h) = value.parse::<Harness>()
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
    /// Harness-specific, like `model`: whatever config or `--permission` said.
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
            append_prompt_file: Some("/proj/main/.agents/pm-baseline.md"),
            prompt: Some("Stand by."),
            resume_session: Some("abc123"),
            fork_session: true,
            permission_mode: Some("acceptEdits"),
            model: Some("opus"),
        });
        assert_eq!(
            cmd,
            "claude --agent reviewer --model 'opus' \
             --append-system-prompt-file '/proj/main/.agents/pm-baseline.md' \
             --permission-mode 'acceptEdits' --resume abc123 --fork-session 'Stand by.'"
        );
    }

    #[test]
    fn project_assets_copies_overwrites_and_never_deletes() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join(".agents");
        let target = tmp.path().join(".claude");
        std::fs::create_dir_all(canonical.join("agents")).unwrap();
        std::fs::create_dir_all(canonical.join("skills/pm")).unwrap();
        std::fs::write(canonical.join("agents/reviewer.md"), "new").unwrap();
        std::fs::write(canonical.join("skills/pm/SKILL.md"), "skill").unwrap();
        std::fs::create_dir_all(target.join("agents")).unwrap();
        std::fs::write(target.join("agents/reviewer.md"), "old").unwrap();
        std::fs::write(target.join("agents/custom.md"), "mine").unwrap();

        // Dry run: reports, writes nothing.
        let dry = Harness::ClaudeCode
            .project_assets(&canonical, &target, true)
            .unwrap();
        assert_eq!(dry.written.len(), 2);
        assert_eq!(dry.replaced, vec![PathBuf::from("agents/reviewer.md")]);
        assert_eq!(
            std::fs::read_to_string(target.join("agents/reviewer.md")).unwrap(),
            "old"
        );
        assert!(!target.join("skills").exists());

        let real = Harness::ClaudeCode
            .project_assets(&canonical, &target, false)
            .unwrap();
        assert_eq!(real, dry);
        assert_eq!(
            std::fs::read_to_string(target.join("agents/reviewer.md")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("skills/pm/SKILL.md")).unwrap(),
            "skill"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("agents/custom.md")).unwrap(),
            "mine"
        );

        // In sync: nothing to do.
        let again = Harness::ClaudeCode
            .project_assets(&canonical, &target, true)
            .unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn harnesses_in_use_is_default_plus_configured() {
        let mut project = AgentsConfig::default();
        project
            .harness
            .insert("implementer".into(), "claude-code".into());
        project.harness.insert("x".into(), "codex".into());
        assert_eq!(
            harnesses_in_use(&project, &AgentsConfig::default()),
            vec![Harness::ClaudeCode]
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
