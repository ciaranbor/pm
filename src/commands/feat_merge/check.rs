//! What a merge needs before it starts: the base checked out, and the
//! feature's and the base's worktrees settled (no uncommitted changes, no
//! paused merge or rebase, whose commits the branch doesn't reach yet). A
//! feature already recorded merged only cleans up, so only its own worktree
//! is checked, and not at all when it is kept.
//!
//! `pm feat merge` refuses with these checks in the CLI's words; `pm serve`
//! runs the same ones to tell a device, in a few words, why its Merge is
//! off ([`super::blocker`]).

use std::path::{Path, PathBuf};

use crate::commands::feat_delete::MissingBase;
use crate::error::{PmError, Result};
use crate::git;
use crate::state::feature::{BaseCheckout, FeatureState, FeatureStatus, base_checkout};
use crate::state::paths;
use crate::state::project::{ProjectConfig, ProjectEntry};

/// A feature's merge as pm state and git set it up.
pub(super) struct Plan {
    pub name: String,
    pub state: FeatureState,
    pub project_name: String,
    pub main_branch: String,
    checkout: std::result::Result<BaseCheckout, MissingBase>,
    pub worktree: PathBuf,
    /// Git still knows the feature's worktree; one it doesn't holds no work
    /// to check, most likely after a cleanup that failed partway.
    pub live: bool,
}

/// Why a merge can't start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    MissingBase(MissingBase),
    /// `base`: the base's worktree, else the feature's.
    Unsettled {
        base: bool,
        why: Unsettled,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unsettled {
    Merging,
    Rebasing,
    Uncommitted,
}

impl Plan {
    pub fn load(project_root: &Path, projects_dir: &Path, name: &str) -> Result<Self> {
        let state = FeatureState::load(&paths::features_dir(project_root), name)?;
        let project_name = ProjectConfig::load(&paths::pm_dir(project_root))?
            .project
            .name;
        let main_branch = ProjectEntry::load(projects_dir, &project_name)?.main_branch;
        let base = state.base_branch(&main_branch);
        let checkout = match base_checkout(project_root, &main_branch, base) {
            Err(PmError::BaseNotCheckedOut(_)) => Err(MissingBase::probe(
                &paths::main_worktree(project_root),
                base,
            )?),
            other => Ok(other?),
        };
        let worktree = project_root.join(&state.worktree);
        let live = match &checkout {
            Ok(checkout) => git::is_worktree(&checkout.worktree, &worktree)?,
            Err(_) => false,
        };
        Ok(Self {
            name: name.to_string(),
            state,
            project_name,
            main_branch,
            checkout,
            worktree,
            live,
        })
    }

    pub fn base(&self) -> &str {
        self.state.base_branch(&self.main_branch)
    }

    pub fn already_merged(&self) -> bool {
        self.state.status == FeatureStatus::Merged
    }

    /// The base's checkout, or why the merge can't start; `keep` when the
    /// feature is to be kept after.
    pub fn check(&self, keep: bool) -> Result<std::result::Result<&BaseCheckout, Refusal>> {
        let checkout = match &self.checkout {
            Ok(checkout) => checkout,
            Err(missing) => return Ok(Err(Refusal::MissingBase(*missing))),
        };
        let feature_checked = self.live && !(self.already_merged() && keep);
        if feature_checked && let Some(why) = unsettled(&self.worktree)? {
            return Ok(Err(Refusal::Unsettled { base: false, why }));
        }
        if !self.already_merged()
            && let Some(why) = unsettled(&checkout.worktree)?
        {
            return Ok(Err(Refusal::Unsettled { base: true, why }));
        }
        Ok(Ok(checkout))
    }

    /// `refusal` as the CLI words it.
    pub fn refuse(&self, refusal: Refusal) -> PmError {
        let name = &self.name;
        let (base, why) = match refusal {
            Refusal::MissingBase(missing) => {
                return missing.refusal(
                    &format!("cannot merge feature '{name}'"),
                    name,
                    self.base(),
                    &self.main_branch,
                );
            }
            Refusal::Unsettled { base, why } => (base, why),
        };
        let (subject, worktree) = match (&self.checkout, base) {
            (Ok(checkout), true) => (
                format!("{} worktree", checkout.scope),
                checkout.worktree.as_path(),
            ),
            _ => (format!("feature '{name}'"), self.worktree.as_path()),
        };
        let action = if self.already_merged() {
            "cleaning up"
        } else {
            "merging"
        };
        let at = worktree.display();
        PmError::SafetyCheck(match why {
            Unsettled::Uncommitted => {
                format!("{subject} has uncommitted changes — commit or stash before {action}")
            }
            Unsettled::Rebasing => format!(
                "{subject} has a rebase in progress in {at}: finish it with \
                 `git rebase --continue` (or `git rebase --abort`) before {action}"
            ),
            Unsettled::Merging => format!(
                "{subject} has a merge in progress in {at}: finish it with \
                 `git commit` (or `git merge --abort`) before {action}"
            ),
        })
    }

    /// `refusal` in a few words, for a device.
    pub fn brief(&self, refusal: Refusal) -> String {
        let base = self.base();
        match refusal {
            Refusal::MissingBase(MissingBase::Gone) => format!("Base {base} is gone"),
            Refusal::MissingBase(MissingBase::NoCheckout) => {
                format!("Base {base} is not checked out")
            }
            Refusal::Unsettled { base, why } => {
                let what = match why {
                    Unsettled::Uncommitted => "Uncommitted changes",
                    Unsettled::Rebasing => "Rebase in progress",
                    Unsettled::Merging => "Merge in progress",
                };
                match (&self.checkout, base) {
                    (Ok(checkout), true) => format!("{what} in {}", checkout.scope),
                    _ => what.to_string(),
                }
            }
        }
    }
}

/// What keeps `worktree`'s work from being merged as it stands. A paused
/// merge or rebase is named before the changes it leaves in the tree.
fn unsettled(worktree: &Path) -> Result<Option<Unsettled>> {
    Ok(if git::merge_in_progress(worktree)? {
        Some(Unsettled::Merging)
    } else if git::rebase_in_progress(worktree)? {
        Some(Unsettled::Rebasing)
    } else if git::has_uncommitted_changes(worktree)? {
        Some(Unsettled::Uncommitted)
    } else {
        None
    })
}
