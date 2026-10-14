//! A harness's trust in pm's hooks.

use super::codex;

/// How a harness stands towards one of pm's hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTrustStatus {
    Trusted,
    /// Never trusted.
    Untrusted,
    /// Trusted in an earlier form: the hook has changed since.
    Modified,
}

/// A harness's trust in the hooks of its user-level file
/// ([`Harness::hook_trust`](super::Harness::hook_trust)).
pub struct HookTrust(pub(super) Option<codex::hook_trust::HookTrust>);

impl HookTrust {
    /// The trust in the hook at `hooks.<event>[entry].hooks[hook]`.
    pub fn status(&self, event: &str, entry: usize, hook: usize) -> HookTrustStatus {
        self.0.as_ref().map_or(HookTrustStatus::Trusted, |trust| {
            trust.status(event, entry, hook)
        })
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::Path;

    use super::codex;

    /// Record codex's trust in the hooks at `positions` (event, entry,
    /// inner hook) of its hooks file under `home`, as codex writes it.
    pub(crate) fn record_codex_hook_trust(home: &Path, positions: &[(&str, usize, usize)]) {
        let codex_home = codex::home_dir(home);
        let hooks_file = codex_home.join(codex::HOOKS_FILE);
        let config = codex_home.join("config.toml");
        let mut text = std::fs::read_to_string(&config).unwrap_or_default();
        for (event, entry, hook) in positions {
            let key = codex::hook_trust_key(&hooks_file, event, *entry, *hook);
            text.push_str(&format!(
                "\n[hooks.state.\"{key}\"]\ntrusted_hash = \"sha256:t\"\n"
            ));
        }
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(config, text).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{Harness, Probe};

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
        let trusted = |harness: Harness, event, entry| {
            harness
                .hook_trust(home.path(), Probe::Cached)
                .status(event, entry, 0)
                == HookTrustStatus::Trusted
        };
        assert!(trusted(Harness::ClaudeCode, "Stop", 0));

        assert!(!Harness::Codex.worktree_trusted(home.path(), wt.path()));
        assert!(
            Harness::Codex
                .trust_worktree(home.path(), wt.path())
                .unwrap()
        );
        assert!(Harness::Codex.worktree_trusted(home.path(), wt.path()));
        assert!(!trusted(Harness::Codex, "Stop", 0));
        let hooks = home.path().join(".codex/hooks.json");
        std::fs::write(
            home.path().join(".codex/config.toml"),
            format!(
                "[hooks.state.\"{}:stop:1:0\"]\ntrusted_hash = \"sha256:x\"\n",
                hooks.display()
            ),
        )
        .unwrap();
        assert!(trusted(Harness::Codex, "Stop", 1));
        assert!(!trusted(Harness::Codex, "Stop", 0));
        assert!(!trusted(Harness::Codex, "SessionStart", 1));
    }
}
