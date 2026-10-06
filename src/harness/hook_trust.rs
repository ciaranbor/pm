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
mod tests {
    use super::*;
    use crate::harness::{Harness, Probe};
    use crate::state::project::HarnessConfig;

    #[test]
    fn trust_seams_are_inert_for_claude_code_and_gate_codex() {
        let home = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let config = HarnessConfig::default();
        assert!(Harness::ClaudeCode.worktree_trusted(home.path(), wt.path()));
        assert!(
            !Harness::ClaudeCode
                .trust_worktree(home.path(), wt.path())
                .unwrap()
        );
        let trusted = |harness: Harness, config: &HarnessConfig, event, entry| {
            harness
                .hook_trust(config, home.path(), Probe::Cached)
                .status(event, entry, 0)
                == HookTrustStatus::Trusted
        };
        assert!(trusted(Harness::ClaudeCode, &config, "Stop", 0));

        assert!(!Harness::Codex.worktree_trusted(home.path(), wt.path()));
        assert!(
            Harness::Codex
                .trust_worktree(home.path(), wt.path())
                .unwrap()
        );
        assert!(Harness::Codex.worktree_trusted(home.path(), wt.path()));
        assert!(!trusted(Harness::Codex, &config, "Stop", 0));
        let hooks = home.path().join(".codex/hooks.json");
        std::fs::write(
            home.path().join(".codex/config.toml"),
            format!(
                "[hooks.state.\"{}:stop:1:0\"]\ntrusted_hash = \"sha256:x\"\n",
                hooks.display()
            ),
        )
        .unwrap();
        assert!(trusted(Harness::Codex, &config, "Stop", 1));
        assert!(!trusted(Harness::Codex, &config, "Stop", 0));
        assert!(!trusted(Harness::Codex, &config, "SessionStart", 1));

        let mut bypassed = HarnessConfig::default();
        bypassed.codex.bypass_hook_trust = Some(true);
        assert!(trusted(Harness::Codex, &bypassed, "SessionStart", 1));
    }
}
