//! The agent harness: the CLI that pm launches inside a tmux window to run an
//! agent. Everything harness-specific — command shape, capability probes,
//! where it reads agent definitions and skills from, trust it needs before it
//! will run pm's hooks — sits behind a `match` on [`Harness`] here, so the
//! spawn chokepoint, the asset installer, the hooks, and the registry stay
//! harness-neutral. Per-agent settings that only the harness can interpret
//! (model id, permission mode) are passed through verbatim.

mod claude_code;
mod codex;

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{PmError, Result};
use crate::fs_utils;
use crate::state::project::{AgentsConfig, HarnessConfig, layered};

/// Which agent CLI a spawn runs on. Dispatched by `match` per seam rather
/// than a trait: the variant set is small and closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    #[default]
    ClaudeCode,
    Codex,
}

impl Harness {
    /// The string form used in config, the registry, and `pm agent list`.
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
        }
    }

    /// Every harness pm can spawn, in the order shown in error messages.
    pub const SUPPORTED: &[Harness] = &[Harness::ClaudeCode, Harness::Codex];

    /// The command line that launches an agent for `spec`, with the
    /// harness's own `[harness.<name>]` settings from `config`.
    pub fn build_cmd(self, spec: &SpawnSpec<'_>, config: &HarnessConfig) -> String {
        match self {
            Harness::ClaudeCode => claude_code::build_cmd(spec),
            Harness::Codex => codex::build_cmd(spec, &config.codex),
        }
    }

    /// Whether the installed harness binary can deliver pm's composed prompt
    /// (the shared baseline and notice boards) to a spawned agent — see
    /// [`prompt_mechanism`](Self::prompt_mechanism). `None` when the binary
    /// can't be probed at all.
    pub fn supports_prompt_delivery(self) -> Option<bool> {
        match self {
            Harness::ClaudeCode => claude_code::supports_append_file(),
            Harness::Codex => codex::version_supported(),
        }
    }

    /// How the composed prompt reaches an agent, for probe and doctor
    /// messages.
    pub fn prompt_mechanism(self) -> String {
        match self {
            Harness::ClaudeCode => {
                "appending a prompt file (--append-system-prompt-file)".to_string()
            }
            Harness::Codex => format!(
                "SessionStart hook context injection (needs codex >= {})",
                codex::min_version_string()
            ),
        }
    }

    /// The harness's own config directory, relative to a worktree or home
    /// (`.claude` for claude-code). Agent definitions and skills are
    /// projected from the canonical `.agents/` store into it.
    pub fn config_dir(self) -> &'static str {
        match self {
            Harness::ClaudeCode => claude_code::CONFIG_DIR,
            Harness::Codex => codex::CONFIG_DIR,
        }
    }

    /// The harness's global config dir under `home` (`~/.claude` for
    /// claude-code, `$CODEX_HOME` or `~/.codex` for codex), where the global
    /// canonical store is projected and the user-level settings live. `None`
    /// for a harness with no global dir — such a harness would need the
    /// global assets projected into each project's own dir instead, which
    /// no current harness requires.
    pub fn global_config_dir(self, home: &Path) -> Option<PathBuf> {
        match self {
            Harness::ClaudeCode => Some(home.join(claude_code::CONFIG_DIR)),
            Harness::Codex => Some(codex::home_dir(home)),
        }
    }

    /// The harness's user-level file that pm installs its hooks into, once
    /// per machine (`~/.claude/settings.json`, `$CODEX_HOME/hooks.json`).
    /// Both take the same nested `hooks` shape. `None` for a harness
    /// without one.
    pub fn user_settings_file(self, home: &Path) -> Option<PathBuf> {
        let dir = self.global_config_dir(home)?;
        Some(match self {
            Harness::ClaudeCode => dir.join(claude_code::USER_SETTINGS_FILE),
            Harness::Codex => dir.join(codex::HOOKS_FILE),
        })
    }

    /// Whether this harness would resolve its *global* copy of skill `name`
    /// over a project one, so a project custom of that name never applies.
    /// Claude Code ranks personal skills above project skills.
    pub fn project_skill_shadowed_by_global(self, home: &Path, name: &str) -> bool {
        match self {
            Harness::ClaudeCode => claude_code::personal_skill_exists(home, name),
            Harness::Codex => false,
        }
    }

    /// Per-worktree files under [`config_dir`](Self::config_dir) that a
    /// feature worktree needs a copy of from main.
    pub fn seeded_files(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => claude_code::SEEDED_FILES,
            Harness::Codex => &[],
        }
    }

    /// Subdirectories of the canonical store this harness projects into
    /// [`config_dir`](Self::config_dir) — and so the ones a feature
    /// worktree needs seeded from both. Empty for a harness that reads the
    /// canonical store itself.
    pub fn projected_dirs(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => claude_code::PROJECTED_DIRS,
            Harness::Codex => &[],
        }
    }

    /// Whether agent definitions must be projected for this harness to see
    /// them — the precondition for a "not projected" finding.
    pub fn projects_definitions(self) -> bool {
        self.projected_dirs().contains(&"agents")
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
            Harness::Codex => Ok(Projection::default()),
        }
    }

    /// Whatever the harness needs recorded before it will run unattended in
    /// `worktree` (codex: the directory-trust entry). Idempotent; returns
    /// whether anything was written.
    pub fn trust_worktree(self, home: &Path, worktree: &Path) -> Result<bool> {
        match self {
            Harness::ClaudeCode => Ok(false),
            Harness::Codex => codex::trust_dir(&codex::home_dir(home), worktree),
        }
    }

    /// Whether `worktree` is already trusted; always true for a harness
    /// without a directory-trust gate.
    pub fn worktree_trusted(self, home: &Path, worktree: &Path) -> bool {
        match self {
            Harness::ClaudeCode => true,
            Harness::Codex => codex::dir_trusted(&codex::home_dir(home), worktree),
        }
    }

    /// Whether the harness has recorded trust for the hook at `hooks.<event>[entry].hooks[hook]`
    /// of its user-level file — the gate without which codex runs no hooks,
    /// silently. Always true for a harness without hook trust.
    pub fn hook_trusted(self, home: &Path, event: &str, entry: usize, hook: usize) -> bool {
        match self {
            Harness::ClaudeCode => true,
            Harness::Codex => {
                let codex_home = codex::home_dir(home);
                let key =
                    codex::hook_trust_key(&codex_home.join(codex::HOOKS_FILE), event, entry, hook);
                codex::hook_trusted(&codex_home, &key)
            }
        }
    }

    /// Events in the harness's hooks file whose entries the harness would
    /// silently register nothing for.
    pub fn malformed_hook_events(self, hooks_root: &serde_json::Value) -> Vec<String> {
        match self {
            Harness::ClaudeCode => Vec::new(),
            Harness::Codex => codex::flat_hook_events(hooks_root),
        }
    }

    /// How a user grants the harness's hook trust, for the doctor finding.
    pub fn hook_trust_remedy(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "",
            Harness::Codex => codex::HOOK_TRUST_REMEDY,
        }
    }

    /// Whether pm's composed prompt reaches this harness through the
    /// SessionStart hook rather than the command line.
    pub fn injects_prompt_at_session_start(self) -> bool {
        match self {
            Harness::ClaudeCode => false,
            Harness::Codex => true,
        }
    }

    /// The SessionStart hook's stdout carrying `context`; `None` for a
    /// harness that does not
    /// [inject there](Self::injects_prompt_at_session_start).
    pub fn session_start_output(self, context: &str) -> Option<String> {
        match self {
            Harness::ClaudeCode => None,
            Harness::Codex => Some(codex::session_start_output(context)),
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
/// `[agents.harness]` resolves to for any configured agent, with the same
/// project-over-global, `""`-masks precedence as a spawn. Unparseable values
/// are skipped here — they error at spawn, where it matters.
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
    /// Directories outside the worktree the agent must be able to write
    /// (pm's state, the shared `.git`) — what a sandboxing harness opens up.
    pub writable_dirs: &'a [PathBuf],
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
        let err = "opencode".parse::<Harness>().unwrap_err().to_string();
        assert_eq!(
            err,
            "harness 'opencode' is not supported yet; supported: claude-code, codex"
        );
        assert_eq!("codex".parse::<Harness>().unwrap(), Harness::Codex);
        assert_eq!(Harness::Codex.to_string(), "codex");
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
        let dirs = vec![PathBuf::from("/proj/.pm")];
        let cmd = Harness::ClaudeCode.build_cmd(
            &SpawnSpec {
                definition: Some("reviewer"),
                append_prompt_file: Some("/proj/main/.agents/pm-baseline.md"),
                prompt: Some("Stand by."),
                resume_session: Some("abc123"),
                fork_session: true,
                permission_mode: Some("acceptEdits"),
                model: Some("opus"),
                writable_dirs: &dirs,
            },
            &HarnessConfig::default(),
        );
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
        project.harness.insert("x".into(), "opencode".into());
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

    #[test]
    fn codex_layout_needs_no_projection_and_hooks_live_in_codex_home() {
        let home = Path::new("/h");
        assert_eq!(
            Harness::Codex.user_settings_file(home),
            Some(PathBuf::from("/h/.codex/hooks.json"))
        );
        assert_eq!(
            Harness::Codex.global_config_dir(home),
            Some(PathBuf::from("/h/.codex"))
        );
        assert!(Harness::Codex.projected_dirs().is_empty());

        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join(".agents");
        std::fs::create_dir_all(canonical.join("agents")).unwrap();
        std::fs::write(canonical.join("agents/reviewer.md"), "x").unwrap();
        let target = tmp.path().join(".codex");
        assert!(
            Harness::Codex
                .project_assets(&canonical, &target, false)
                .unwrap()
                .is_empty()
        );
        assert!(!target.exists());
    }

    #[test]
    fn trust_seams_are_inert_for_claude_code_and_gate_codex() {
        let home = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        assert!(Harness::ClaudeCode.worktree_trusted(home.path(), wt.path()));
        assert!(
            !Harness::ClaudeCode
                .trust_worktree(home.path(), wt.path())
                .unwrap()
        );
        assert!(Harness::ClaudeCode.hook_trusted(home.path(), "Stop", 0, 0));

        assert!(!Harness::Codex.worktree_trusted(home.path(), wt.path()));
        assert!(
            Harness::Codex
                .trust_worktree(home.path(), wt.path())
                .unwrap()
        );
        assert!(Harness::Codex.worktree_trusted(home.path(), wt.path()));
        assert!(!Harness::Codex.hook_trusted(home.path(), "Stop", 0, 0));
        let hooks = home.path().join(".codex/hooks.json");
        std::fs::write(
            home.path().join(".codex/config.toml"),
            format!(
                "[hooks.state.\"{}:stop:1:0\"]\ntrusted_hash = \"sha256:x\"\n",
                hooks.display()
            ),
        )
        .unwrap();
        assert!(Harness::Codex.hook_trusted(home.path(), "Stop", 1, 0));
        assert!(!Harness::Codex.hook_trusted(home.path(), "Stop", 0, 0));
        assert!(!Harness::Codex.hook_trusted(home.path(), "SessionStart", 1, 0));
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
