//! A feature's base branch: the scope to land in once it is gone, and why
//! it may have no checkout.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::git;
use crate::state::feature::base_checkout;

/// The scope to land in after a feature's session is killed: that of its
/// `base` branch, or `main` when the branch has no checkout (a parent feature
/// deleted first).
pub fn base_scope(project_root: &Path, main_branch: &str, base: &str) -> String {
    base_checkout(project_root, main_branch, base)
        .map(|c| c.scope)
        .unwrap_or_else(|_| "main".to_string())
}

/// Why a feature's base branch has no checkout in this project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingBase {
    /// The branch no longer exists: the parent feature was merged or deleted.
    Gone,
    /// The branch exists but is neither the main branch nor a feature's.
    NoCheckout,
}

impl MissingBase {
    pub fn probe(main_worktree: &Path, base: &str) -> Result<Self> {
        Ok(if git::branch_exists(main_worktree, base)? {
            Self::NoCheckout
        } else {
            Self::Gone
        })
    }

    /// The situation, as an error-message clause.
    pub fn reason(self, base: &str) -> String {
        match self {
            Self::Gone => format!(
                "base branch '{base}' is gone (the feature it was stacked on was merged or deleted)"
            ),
            Self::NoCheckout => format!(
                "base branch '{base}' is not checked out in this project (it is neither the main branch nor a feature's)"
            ),
        }
    }

    /// The refusal of a command that needs the base checked out, `cannot`
    /// saying which: [`Self::reason`] plus the way out. pm never guesses a
    /// replacement base, so the choice is the user's.
    pub fn refusal(self, cannot: &str, feature: &str, base: &str, main_branch: &str) -> PmError {
        let reason = format!("{cannot}: {}.", self.reason(base));
        let (cli, remote) = match self {
            Self::Gone => (
                format!(
                    "Rebase onto a live branch (`git rebase {main_branch}` in the worktree) \
                     and set `base = \"{main_branch}\"` in .pm/features/{feature}.toml, \
                     or discard the feature with `pm feat delete --force {feature}`."
                ),
                format!(
                    "At a terminal, rebase it onto a live branch and set its base, \
                     or discard it with `pm feat delete --force {feature}`."
                ),
            ),
            Self::NoCheckout => (
                format!("Give it one with `pm feat adopt {base}`, then retry."),
                format!("Give it one with `pm feat adopt {base}` at a terminal, then retry."),
            ),
        };
        PmError::Unsafe {
            reason,
            cli,
            remote,
        }
    }
}
