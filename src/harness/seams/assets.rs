//! Asset seams: the harness's config dirs and how the canonical
//! `.agents/` store is projected into them.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::harness::{Harness, Projection, ProjectionScope, claude_code, codex, opencode};

impl Harness {
    /// The harness's own config directory, relative to a worktree or home
    /// (`.claude` for claude-code). Agent definitions and skills are
    /// projected from the canonical `.agents/` store into it.
    pub fn config_dir(self) -> &'static str {
        match self {
            Harness::ClaudeCode => claude_code::CONFIG_DIR,
            Harness::Codex => codex::CONFIG_DIR,
            Harness::OpenCode => opencode::CONFIG_DIR,
        }
    }

    /// The harness's global config dir under `home` (`~/.claude` for
    /// claude-code, `$CODEX_HOME` or `~/.codex` for codex,
    /// `$XDG_CONFIG_HOME/opencode` or `~/.config/opencode` for opencode), where the global
    /// canonical store is projected and the user-level settings live. `None`
    /// for a harness with no global dir — such a harness would need the
    /// global assets projected into each project's own dir instead, which
    /// no current harness requires.
    pub fn global_config_dir(self, home: &Path) -> Option<PathBuf> {
        match self {
            Harness::ClaudeCode => Some(home.join(claude_code::CONFIG_DIR)),
            Harness::Codex => Some(codex::home_dir(home)),
            Harness::OpenCode => Some(opencode::global_dir(home)),
        }
    }

    /// Per-worktree files under [`config_dir`](Self::config_dir) that a
    /// feature worktree needs a copy of from main: the project's own
    /// settings and permissions, never hooks (those are user-level).
    pub fn seeded_files(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => claude_code::SEEDED_FILES,
            Harness::Codex | Harness::OpenCode => &[],
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
            Harness::OpenCode => opencode::PROJECTED_DIRS,
        }
    }

    /// Whether agent definitions must be projected for this harness to see
    /// them — the precondition for a "not projected" finding.
    pub fn projects_definitions(self) -> bool {
        self.projected_dirs().contains(&"agents")
    }

    /// Whether the harness, started in `worktree`, finds `definition`: its
    /// projected copy is in the worktree's config dir or the global one.
    /// Always true for a harness that reads the canonical store itself.
    pub fn definition_projected(self, worktree: &Path, home: &Path, definition: &str) -> bool {
        if !self.projects_definitions() {
            return true;
        }
        let file = format!("{definition}.md");
        [
            Some(worktree.join(self.config_dir())),
            self.global_config_dir(home),
        ]
        .into_iter()
        .flatten()
        .any(|dir| dir.join("agents").join(&file).is_file())
    }

    /// Project the canonical asset store (`<canonical_root>/{agents,skills}`)
    /// into this harness's own layout under `target_root`, limited to
    /// `scope`. Copies over same-named files and never deletes anything in
    /// the target. In `dry_run` nothing is written; the report still says
    /// what would be.
    pub fn project_assets(
        self,
        canonical_root: &Path,
        target_root: &Path,
        scope: &ProjectionScope<'_>,
        dry_run: bool,
    ) -> Result<Projection> {
        match self {
            Harness::ClaudeCode => {
                claude_code::project_assets(canonical_root, target_root, scope, dry_run)
            }
            Harness::Codex => Ok(Projection::default()),
            Harness::OpenCode => {
                opencode::project_assets(canonical_root, target_root, scope, dry_run)
            }
        }
    }

    /// Whether this harness would resolve its *global* copy of skill `name`
    /// over a project one, so a project custom of that name never applies.
    /// Claude Code ranks personal skills above project skills.
    pub fn project_skill_shadowed_by_global(self, home: &Path, name: &str) -> bool {
        match self {
            Harness::ClaudeCode => claude_code::personal_skill_exists(home, name),
            Harness::Codex | Harness::OpenCode => false,
        }
    }
}
