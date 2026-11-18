//! Merging a PR on GitHub: what the PR and its repo allow, and the merge.

use std::path::Path;

use serde::Deserialize;

use super::run_gh;
use crate::error::{PmError, Result};

/// How GitHub lands a PR on its base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub const ALL: [MergeMethod; 3] = [Self::Merge, Self::Squash, Self::Rebase];

    /// The method a config value names, case-insensitively.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|method| method.name().eq_ignore_ascii_case(value))
    }

    /// The name in config and in `gh pr merge`'s flag.
    pub fn name(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }
}

/// The merge settings of the repo `gh` resolves from a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMergeSettings {
    pub allowed: Vec<MergeMethod>,
    /// The method GitHub preselects for the authenticated user.
    pub default: Option<MergeMethod>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoJson {
    merge_commit_allowed: bool,
    squash_merge_allowed: bool,
    rebase_merge_allowed: bool,
    #[serde(default)]
    viewer_default_merge_method: String,
}

pub fn repo_merge_settings(repo_dir: &Path) -> Result<RepoMergeSettings> {
    let output = run_gh(
        repo_dir,
        &[
            "repo",
            "view",
            "--json",
            "mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed,viewerDefaultMergeMethod",
        ],
    )?;
    let repo: RepoJson = serde_json::from_str(&output)
        .map_err(|e| PmError::Gh(format!("parse repo merge settings: {e}")))?;
    let allowed = [
        (MergeMethod::Merge, repo.merge_commit_allowed),
        (MergeMethod::Squash, repo.squash_merge_allowed),
        (MergeMethod::Rebase, repo.rebase_merge_allowed),
    ]
    .into_iter()
    .filter_map(|(method, allowed)| allowed.then_some(method))
    .collect();
    Ok(RepoMergeSettings {
        allowed,
        default: MergeMethod::parse(&repo.viewer_default_merge_method),
    })
}

/// What decides whether a PR can be merged now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrMergeState {
    /// "OPEN", "MERGED", or "CLOSED"
    pub state: String,
    pub is_draft: bool,
    /// "MERGEABLE", "CONFLICTING", or "UNKNOWN" while GitHub computes it.
    pub mergeable: String,
    pub head_oid: String,
    pub base_ref: String,
    /// The checks and commit statuses that failed, by name.
    pub failing_checks: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrJson {
    state: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    mergeable: String,
    #[serde(default)]
    head_ref_oid: String,
    #[serde(default)]
    base_ref_name: String,
    #[serde(default)]
    status_check_rollup: Vec<serde_json::Value>,
}

/// Conclusions of a check run, and states of a commit status, that fail it.
const FAILING: &[&str] = &[
    "FAILURE",
    "ERROR",
    "CANCELLED",
    "TIMED_OUT",
    "ACTION_REQUIRED",
    "STARTUP_FAILURE",
];

pub fn pr_merge_state(repo_dir: &Path, pr: &str) -> Result<PrMergeState> {
    let output = run_gh(
        repo_dir,
        &[
            "pr",
            "view",
            pr,
            "--json",
            "state,isDraft,mergeable,headRefOid,baseRefName,statusCheckRollup",
        ],
    )?;
    let parsed: PrJson = serde_json::from_str(&output)
        .map_err(|e| PmError::Gh(format!("parse PR merge state: {e}")))?;
    let failing_checks = parsed
        .status_check_rollup
        .iter()
        .filter(|check| {
            ["conclusion", "state"].iter().any(|key| {
                check[key]
                    .as_str()
                    .is_some_and(|v| FAILING.contains(&v.to_uppercase().as_str()))
            })
        })
        .map(|check| {
            check["name"]
                .as_str()
                .or(check["context"].as_str())
                .unwrap_or("unnamed check")
                .to_string()
        })
        .collect();
    Ok(PrMergeState {
        state: parsed.state.to_uppercase(),
        is_draft: parsed.is_draft,
        mergeable: parsed.mergeable.to_uppercase(),
        head_oid: parsed.head_ref_oid,
        base_ref: parsed.base_ref_name,
        failing_checks,
    })
}

/// Merge PR `pr` by `method`, only while its head is still `head_oid`.
/// Never `--delete-branch`: gh's local cleanup fails inside a linked
/// worktree and exits non-zero after the merge succeeded (cli/cli#13955).
pub fn merge_pr(repo_dir: &Path, pr: &str, method: MergeMethod, head_oid: &str) -> Result<()> {
    let flag = format!("--{}", method.name());
    run_gh(
        repo_dir,
        &["pr", "merge", pr, &flag, "--match-head-commit", head_oid],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::fake;

    #[test]
    fn failing_checks_name_failed_runs_and_statuses_but_not_pending_or_passing_ones() {
        let _gh = fake::install(|_| {
            Ok(r#"{"state":"OPEN","isDraft":false,"mergeable":"MERGEABLE",
                "headRefOid":"abc","baseRefName":"main","statusCheckRollup":[
                {"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"},
                {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"SUCCESS"},
                {"__typename":"CheckRun","name":"slow","status":"IN_PROGRESS","conclusion":""},
                {"__typename":"StatusContext","context":"ci/legacy","state":"ERROR"},
                {"__typename":"StatusContext","context":"ci/ok","state":"SUCCESS"}]}"#
                .to_string())
        });
        let pr = pr_merge_state(Path::new("."), "7").unwrap();
        assert_eq!(pr.failing_checks, ["test", "ci/legacy"]);
    }

    #[test]
    fn repo_settings_list_only_allowed_methods() {
        let _gh = fake::install(|_| {
            Ok(r#"{"mergeCommitAllowed":false,"squashMergeAllowed":true,
                "rebaseMergeAllowed":true,"viewerDefaultMergeMethod":"SQUASH"}"#
                .to_string())
        });
        let repo = repo_merge_settings(Path::new(".")).unwrap();
        assert_eq!(repo.allowed, [MergeMethod::Squash, MergeMethod::Rebase]);
        assert_eq!(repo.default, Some(MergeMethod::Squash));
    }
}
