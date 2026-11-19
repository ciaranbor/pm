//! A PR's CI checks, read from its status check rollup: GitHub Actions
//! check runs and legacy commit statuses alike.

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::run_gh;
use crate::error::{PmError, Result};

/// Conclusions of a check run, and states of a commit status, that fail it.
const FAILING: &[&str] = &[
    "FAILURE",
    "ERROR",
    "CANCELLED",
    "TIMED_OUT",
    "ACTION_REQUIRED",
    "STARTUP_FAILURE",
];

/// Commit status states that are still waiting on a result.
const PENDING_STATES: &[&str] = &["PENDING", "EXPECTED"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Passed, skipped or neutral.
    Pass,
    Fail,
    Pending,
}

/// What a rollup entry, a check run or a commit status, came to.
pub(super) fn outcome(check: &Value) -> Outcome {
    let upper = |key: &str| check[key].as_str().map(str::to_uppercase);
    let conclusion = upper("conclusion");
    let state = upper("state");
    if [&conclusion, &state]
        .into_iter()
        .flatten()
        .any(|v| FAILING.contains(&v.as_str()))
    {
        return Outcome::Fail;
    }
    let running = upper("status").is_some_and(|s| !s.is_empty() && s != "COMPLETED");
    let waiting = state.is_some_and(|s| PENDING_STATES.contains(&s.as_str()));
    if running || waiting {
        Outcome::Pending
    } else {
        Outcome::Pass
    }
}

/// A rollup entry's name: a check run's, else a commit status's context.
pub(super) fn name(check: &Value) -> String {
    check["name"]
        .as_str()
        .or(check["context"].as_str())
        .unwrap_or("unnamed check")
        .to_string()
}

/// A PR's state, head commit and checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrChecks {
    /// "OPEN", "MERGED", or "CLOSED"
    pub state: String,
    pub head_oid: String,
    pub checks: Vec<(String, Outcome)>,
}

impl PrChecks {
    /// The names of the checks with `outcome`, in rollup order.
    pub fn named(&self, outcome: Outcome) -> Vec<&str> {
        self.checks
            .iter()
            .filter(|(_, o)| *o == outcome)
            .map(|(n, _)| n.as_str())
            .collect()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrJson {
    state: String,
    #[serde(default)]
    head_ref_oid: String,
    #[serde(default)]
    status_check_rollup: Vec<Value>,
}

/// PR `pr`'s checks on its current head. A PR with no CI has none.
pub fn pr_checks(repo_dir: &Path, pr: &str) -> Result<PrChecks> {
    let output = run_gh(
        repo_dir,
        &[
            "pr",
            "view",
            pr,
            "--json",
            "state,headRefOid,statusCheckRollup",
        ],
    )?;
    let parsed: PrJson =
        serde_json::from_str(&output).map_err(|e| PmError::Gh(format!("parse PR checks: {e}")))?;
    Ok(PrChecks {
        state: parsed.state.to_uppercase(),
        head_oid: parsed.head_ref_oid,
        checks: parsed
            .status_check_rollup
            .iter()
            .map(|c| (name(c), outcome(c)))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::fake;

    fn checks(rollup: &str) -> PrChecks {
        let json =
            format!(r#"{{"state":"OPEN","headRefOid":"abc","statusCheckRollup":[{rollup}]}}"#);
        let _gh = fake::install(move |_| Ok(json.clone()));
        pr_checks(Path::new("."), "7").unwrap()
    }

    #[test]
    fn runs_and_statuses_sort_into_pass_fail_and_pending() {
        let pr = checks(
            r#"{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"},
            {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"SUCCESS"},
            {"__typename":"CheckRun","name":"docs","status":"COMPLETED","conclusion":"SKIPPED"},
            {"__typename":"CheckRun","name":"slow","status":"IN_PROGRESS","conclusion":""},
            {"__typename":"CheckRun","name":"queued","status":"QUEUED","conclusion":""},
            {"__typename":"CheckRun","name":"stopped","status":"COMPLETED","conclusion":"CANCELLED"},
            {"__typename":"StatusContext","context":"ci/legacy","state":"ERROR"},
            {"__typename":"StatusContext","context":"ci/wait","state":"PENDING"},
            {"__typename":"StatusContext","context":"ci/ok","state":"SUCCESS"}"#,
        );
        assert_eq!(pr.named(Outcome::Fail), ["test", "stopped", "ci/legacy"]);
        assert_eq!(pr.named(Outcome::Pending), ["slow", "queued", "ci/wait"]);
        assert_eq!(pr.named(Outcome::Pass), ["lint", "docs", "ci/ok"]);
        assert_eq!(pr.head_oid, "abc");
    }

    #[test]
    fn a_pr_without_ci_has_no_checks() {
        assert!(checks("").checks.is_empty());
    }
}
