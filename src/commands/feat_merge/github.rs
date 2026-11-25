//! A feature with a linked PR merges on GitHub, not locally. The PR's state
//! on GitHub is the truth: one already merged there is not merged again, so
//! a retry after any failure only finishes the job, and git ancestry, which
//! can't see a squash or rebase merge, is never asked. A merged PR is
//! cleaned up only if it merged into the feature's base with every commit
//! of the local branch, as cleanup deletes that branch; so is a feature
//! already recorded merged, however that was learnt.
//!
//! An open PR merges only when it is not a draft, targets the feature's
//! base, has no conflicts and no failing checks, and its head is the local
//! branch's commit (cleanup deletes the local branch, so commits GitHub
//! lacks would be lost). The method is `[github] merge_method`, else the
//! one GitHub preselects for the user, and must be one the repo allows.
//! GitHub may accept a merge without landing it (auto-merge or a merge
//! queue); that is reported, and nothing is cleaned up until it lands.
//!
//! The PR's remote branch is deleted separately, never through gh's
//! `--delete-branch` (see [`gh::merge_pr`]).

use std::path::Path;

use crate::commands::feat_delete::unshipped;
use crate::error::{PmError, Result};
use crate::gh::{self, MergeMethod};
use crate::git;
use crate::state::paths;
use crate::state::project::ProjectConfig;

use super::check::Plan;

/// Merge `plan`'s PR on GitHub from the base's checkout `repo`, unless it
/// already merged there.
pub(super) fn land(project_root: &Path, repo: &Path, plan: &Plan) -> Result<()> {
    let name = &plan.name;
    let number = &plan.state.pr;
    let branch = &plan.state.branch;
    let base = plan.base();
    let pr = gh::pr_merge_state(repo, number)?;
    match pr.state.as_str() {
        "MERGED" => {
            if pr.base_ref != base {
                return Err(PmError::SafetyCheck(format!(
                    "PR #{number} was merged into '{}', not feature '{name}''s base '{base}'{}",
                    pr.base_ref,
                    way_out(plan)
                )));
            }
            ensure_shipped(repo, plan, &pr.head_oid)?;
            eprintln!("PR #{number} is already merged on GitHub — cleaning up");
            return Ok(());
        }
        "OPEN" => {}
        other => {
            return Err(PmError::SafetyCheck(format!(
                "PR #{number} of feature '{name}' is {}, not open — reopen it on GitHub, \
                 or end the feature with `pm feat delete`",
                other.to_lowercase()
            )));
        }
    }
    let refuse = |why: String| {
        Err(PmError::SafetyCheck(format!(
            "cannot merge PR #{number}: {why}"
        )))
    };
    if pr.is_draft {
        return refuse(format!(
            "it is a draft — mark it ready with `pm feat pr ready {name}` first"
        ));
    }
    if pr.base_ref != base {
        return refuse(format!(
            "it targets '{}', but feature '{name}' is based on '{base}'",
            pr.base_ref
        ));
    }
    if pr.mergeable == "CONFLICTING" {
        return refuse(format!(
            "it conflicts with '{base}' — rebase feature '{name}' onto it and push"
        ));
    }
    if !pr.failing_checks.is_empty() {
        return refuse(format!(
            "checks are failing: {}",
            pr.failing_checks.join(", ")
        ));
    }
    if git::branch_commit(repo, branch)?.as_deref() != Some(pr.head_oid.as_str()) {
        return refuse(format!(
            "its head is not branch '{branch}' as it stands here — push the branch, \
             or pull what GitHub has, first"
        ));
    }
    let method = method(project_root, repo)?;

    let merged = gh::merge_pr(repo, number, method, &pr.head_oid);
    let reread = gh::pr_merge_state(repo, number).map(|pr| pr.state);
    match (merged, reread) {
        (_, Ok(state)) if state == "MERGED" => Ok(()),
        (Err(e), _) => Err(e),
        (Ok(()), Err(e)) => Err(e),
        (Ok(()), Ok(_)) => Err(PmError::SafetyCheck(format!(
            "GitHub accepted PR #{number} but has not merged it yet (auto-merge or a merge \
             queue) — run `pm feat merge {name}` again once it has"
        ))),
    }
}

/// Refuses a cleanup of `plan`'s feature, recorded merged, that would lose
/// commits of its local branch the PR lacks. GitHub is asked only while
/// that branch exists and its base doesn't already have it.
pub(super) fn ensure_recorded_shipped(repo: &Path, plan: &Plan) -> Result<()> {
    let (name, branch) = (&plan.name, &plan.state.branch);
    if git::branch_commit(repo, branch)?.is_none()
        || git::branch_merged_into(repo, branch, plan.base())?
    {
        return Ok(());
    }
    let pr = gh::pr_merge_state(repo, &plan.state.pr).map_err(|e| {
        PmError::SafetyCheck(format!(
            "cannot check that PR #{} has every commit of branch '{branch}', which cleanup \
             deletes: {e} — end the feature anyway with `pm feat delete --force {name}`",
            plan.state.pr
        ))
    })?;
    ensure_shipped(repo, plan, &pr.head_oid)
}

/// Refuses unless `plan`'s local branch is wholly in `head`, the commit its
/// PR merged at, as cleanup deletes the branch.
fn ensure_shipped(repo: &Path, plan: &Plan, head: &str) -> Result<()> {
    let state = &plan.state;
    match unshipped(repo, &state.branch, &state.pr, head, &way_out(plan))? {
        None => Ok(()),
        Some(loss) => Err(PmError::SafetyCheck(loss)),
    }
}

/// What to do about work a merged PR lacks.
fn way_out(plan: &Plan) -> String {
    let name = &plan.name;
    format!(
        " — open a new PR for the rest with `pm feat pr create {name}`, \
         or end the feature with `pm feat delete --force {name}`"
    )
}

/// The method to merge with: the configured one, else GitHub's default for
/// the user, refused unless the repo allows it.
fn method(project_root: &Path, repo: &Path) -> Result<MergeMethod> {
    let configured = ProjectConfig::load(&paths::pm_dir(project_root))?
        .github
        .merge_method;
    let configured = match configured.as_deref() {
        None => None,
        Some(value) => Some(MergeMethod::parse(value).ok_or_else(|| {
            PmError::SafetyCheck(format!(
                "github.merge_method is '{value}'; it must be squash, merge or rebase \
                 (`pm config set github.merge_method <method>`)"
            ))
        })?),
    };
    let repo = gh::repo_merge_settings(repo)?;
    let allowed = |method: &MergeMethod| repo.allowed.contains(method);
    match configured {
        Some(method) if allowed(&method) => Ok(method),
        Some(method) => Err(PmError::SafetyCheck(format!(
            "github.merge_method is '{}', which this GitHub repo does not allow; it allows {} \
             (`pm config set github.merge_method <method>`)",
            method.name(),
            names(&repo.allowed)
        ))),
        None => repo
            .default
            .filter(allowed)
            .or_else(|| repo.allowed.first().copied())
            .ok_or_else(|| PmError::Gh("the GitHub repo allows no merge method".to_string())),
    }
}

fn names(methods: &[MergeMethod]) -> String {
    methods
        .iter()
        .map(|m| m.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Delete the PR's branch from the remote it was pushed to; a warning when
/// that fails, and nothing when it is already gone.
pub(super) fn delete_remote_branch(repo: &Path, branch: &str) -> Option<String> {
    let remote = match git::push_remote(repo, branch) {
        Ok(remote) => remote,
        Err(e) => {
            return Some(format!(
                "could not delete the remote branch '{branch}': {e}"
            ));
        }
    };
    match git::delete_remote_branch(repo, &remote, branch) {
        Ok(()) => None,
        Err(PmError::Git(stderr)) if stderr.contains("remote ref does not exist") => None,
        Err(e) => Some(format!(
            "could not delete branch '{branch}' from '{remote}': {e}"
        )),
    }
}
